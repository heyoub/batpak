mod ancestry;
mod append;
mod backup_envelope;
mod chain_walk;
pub(crate) mod cold_start;
mod compaction_report;
mod config;
/// Push subscriptions (lossy) and pull cursors (ordered, with optional durable
/// checkpoints) for event delivery.
pub mod delivery;
mod dir_lock;
mod error;
/// Fault injection framework for testing failure scenarios.
#[cfg(feature = "dangerous-test-hooks")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "dangerous-test-hooks"))
)]
pub mod fault;
mod gate;
mod hidden_ranges;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
mod lifecycle;
mod platform;
/// Projection cache traits and built-in backends (NoCache, NativeCache).
pub mod projection;
mod projection_run;
/// Typed reactor output batch — accumulator handed to typed reactor handlers.
pub mod reaction;
/// Typed reactor public surface + shared internal canal runner.
pub mod reactor_typed;
mod read_walk;
#[cfg(test)]
mod runtime_contracts;
/// On-disk segment format, frame encoding/decoding, and compaction helpers.
pub mod segment;
mod signing;
/// Runtime statistics and diagnostic snapshots.
pub mod stats;
mod store_resource_report;
mod subscriber_frontier;
#[cfg(feature = "dangerous-test-hooks")]
mod test_support;
pub(crate) mod write;

pub use append::{
    AppendOptions, AppendPositionHint, AppendReceipt, BatchAppendItem, CausationRef,
    CompactionConfig, CompactionStrategy, DenialReceipt, EncodedBytes, ExtensionKey,
    ExtensionKeyError, RetentionPredicate,
};
pub use backup_envelope::{
    audit_backup_manifest_segments, backup_manifest_body_bytes, backup_manifest_body_hash,
    normalize_backup_manifest_body, restore_proof_report_body, restore_proof_report_body_hash,
    sort_backup_segment_refs, verify_backup_manifest_envelope,
    verify_backup_manifest_signatures_only, BackupEnvelope, BackupEnvelopeFinding,
    BackupManifestBody, BackupManifestEnvelope, BackupManifestVerification, BackupSegmentRef,
    RestoreProofEvidenceReport, RestoreProofReportBody, SegmentBytesDigest,
    BACKUP_MANIFEST_BODY_SCHEMA_VERSION, RESTORE_PROOF_REPORT_SCHEMA_VERSION,
};
pub use chain_walk::{
    ChainWalkEvidenceReport, ChainWalkFinding, ChainWalkHash, ChainWalkMode, ChainWalkReportBody,
    ChainWalkReportError, ChainWalkRequest, ChainWalkStartRef, CHAIN_WALK_REPORT_SCHEMA_VERSION,
};
pub use cold_start::rebuild::{OpenIndexPath, OpenIndexReport};
pub use compaction_report::{
    compaction_strategy_shape, report_for_run, report_skipped, CompactionReportBody,
    CompactionReportFinding, CompactionStrategyShape, COMPACTION_REPORT_SCHEMA_VERSION,
};
pub use config::{
    BatchConfig, IndexConfig, IndexTopology, OpenReportObserver, StoreConfig, SyncConfig, SyncMode,
    WriterConfig,
};
pub use delivery::cursor::{
    Cursor, CursorGapConfig, CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle,
    GapObservation,
};
pub use delivery::observation::{AtLeastOnce, CheckpointId, IdempotencyKey, ObservedOnce};
pub use delivery::subscription::Subscription;
pub use error::{StoreError, StoreLockMode};
#[cfg(feature = "dangerous-test-hooks")]
#[cfg_attr(
    all(docsrs, not(batpak_stable_docs)),
    doc(cfg(feature = "dangerous-test-hooks"))
)]
pub use fault::{
    CountdownAction, CountdownInjector, FaultInjector, InjectionPoint, ProbabilisticInjector,
};
pub use gate::DurabilityGate;
pub use index::{ClockKey, DiskPos, IndexEntry};
pub use projection::watch::{ProjectionWatcher, WatcherError};
pub use projection::{
    CacheCapabilities, CacheMeta, Freshness, NativeCache, NoCache, ProjectionCache,
};
pub use projection_run::{
    ProjectionRunCacheStatus, ProjectionRunCheckpointRef, ProjectionRunEvidenceReport,
    ProjectionRunFinding, ProjectionRunFreshnessStatus, ProjectionRunFrontierKind,
    ProjectionRunHash, ProjectionRunInputFrontier, ProjectionRunOutputHash,
    ProjectionRunReplayMode, ProjectionRunReportBody, ProjectionRunReportError,
    ProjectionRunRequestedFreshness, ProjectionSourceRef, PROJECTION_RUN_REPORT_SCHEMA_VERSION,
};
pub use reaction::ReactionBatch;
pub use reactor_typed::{ReactorConfig, ReactorError, TypedReactorHandle};
pub use read_walk::{
    ReadWalkDroppedCount, ReadWalkEvidenceReport, ReadWalkFinding, ReadWalkFreshnessIntent,
    ReadWalkFrontierKind, ReadWalkHash, ReadWalkInputFrontier, ReadWalkProofRef, ReadWalkProofRefs,
    ReadWalkReplayMode, ReadWalkReportBody, ReadWalkReportError, ReadWalkRequest,
    ReadWalkSourceRef, READ_WALK_REPORT_SCHEMA_VERSION,
};
pub use signing::SigningKey;
pub use stats::{
    ActiveSegmentReadEvidence, ClockEvidence, FrontierView, HlcPoint, HostEvidenceSummary,
    LockLeafSymlinkProtection, MmapAdmissionSummary, MmapEvidence, ParentDirSyncAdmissionSummary,
    ParentDirSyncEvidence, PlatformAdmissionSummary, PlatformEvidenceSummary, StoreDiagnostics,
    StoreLockAdmissionSummary, StorePathEvidenceSummary, StorePathStatusEvidence, StoreStats,
    WatermarkKind, WatermarkSnapshot, WriterPressure,
};
pub use store_resource_report::{
    store_data_dir_identity_hash, store_resource_evidence_report_from_diagnostics,
    store_resource_report_body_from_diagnostics, store_resource_report_body_hash,
    StoreResourceEnvelope, StoreResourceEvidenceReport, StoreResourceFrontierBody,
    StoreResourceHash, StoreResourceReportBody, StoreResourceReportError,
    StoreResourceRestartPolicyShape, STORE_RESOURCE_REPORT_SCHEMA_VERSION,
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
#[cfg(test)]
pub(crate) use config::now_us;
use index::StoreIndex;
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

struct OpenComponents {
    runtime: Arc<config::ValidatedStoreConfig>,
    config: Arc<StoreConfig>,
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    open_report: cold_start::rebuild::OpenIndexReport,
    cumulative_reserved_kind_fallbacks: segment::sidx::ReservedKindFallbackStats,
    store_lock: dir_lock::StoreDirLock,
}

fn generation_advanced_after_subscribe(baseline: u64, post_subscribe: u64) -> bool {
    post_subscribe > baseline
}

fn validate_payload_registry_for_open(config: &StoreConfig) -> Result<(), StoreError> {
    let Err(error) = event::payload::cached_event_payload_registry_validation() else {
        return Ok(());
    };
    match config.event_payload_validation {
        EventPayloadValidation::Warn => {
            if event::payload::mark_event_payload_registry_warning_emitted() {
                tracing::warn!(
                    target: "batpak::event_registry",
                    collisions = ?error.collisions(),
                    "duplicate EventPayload kind registrations detected; call validate_event_payload_registry() or set EventPayloadValidation::FailFast to make this an open error"
                );
            }
            Ok(())
        }
        EventPayloadValidation::FailFast => Err(StoreError::EventPayloadRegistry(error)),
        EventPayloadValidation::Silent => Ok(()),
    }
}

fn open_components(
    mut config: StoreConfig,
    lock_mode: StoreLockMode,
) -> Result<OpenComponents, StoreError> {
    validate_payload_registry_for_open(&config)?;
    std::fs::create_dir_all(&config.data_dir)?;
    config.data_dir = std::fs::canonicalize(&config.data_dir).map_err(StoreError::Io)?;
    let configured_signing_keys = config.signing_keys.len();
    tracing::debug!(
        configured_signing_keys,
        "opening store with configured signing registry"
    );
    let runtime = Arc::new(config.validated()?);
    let store_lock = dir_lock::StoreDirLock::acquire(&config.data_dir, lock_mode)?;
    if let Some(profile_path) = config.platform_profile_path.as_ref() {
        let _verified_platform_evidence =
            platform::profile::PlatformProfile::verify_current_store_path(
                profile_path,
                &config.data_dir,
            )?;
    }
    let config = Arc::new(config);
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

    // Cold start: checkpoint/mmap fast paths or full segment scan.
    // Segment files are named so lexicographic order matches replay order.
    let open_outcome =
        cold_start::rebuild::open_index(&index, &reader, &config.data_dir, runtime.cold_start)?;

    // Tell the reader which segment is active (for mmap dispatch).
    // The writer's initial segment ID is the highest existing + 1.
    let active_seg_id = next_active_segment_id(&config.data_dir);
    reader.set_active_segment(active_seg_id);

    Ok(OpenComponents {
        runtime,
        config,
        index,
        reader,
        open_report: open_outcome.report,
        cumulative_reserved_kind_fallbacks: open_outcome.cumulative_reserved_kind_fallbacks,
        store_lock,
    })
}

fn next_active_segment_id(data_dir: &std::path::Path) -> u64 {
    write::writer::find_latest_segment_id(data_dir).unwrap_or(0) + 1
}

fn emit_open_report_observability(config: &StoreConfig, report: &OpenIndexReport) {
    tracing::info!(
        target: "batpak::open",
        path = ?report.path,
        restored_entries = report.restored_entries,
        tail_entries = report.tail_entries,
        elapsed_us = report.elapsed_us,
        unknown_reserved_system_kind_fallbacks = report.unknown_reserved_system_kind_fallbacks,
        unknown_reserved_effect_kind_fallbacks = report.unknown_reserved_effect_kind_fallbacks,
        cumulative_unknown_reserved_system_kind_fallbacks = report
            .cumulative_unknown_reserved_system_kind_fallbacks,
        cumulative_unknown_reserved_effect_kind_fallbacks = report
            .cumulative_unknown_reserved_effect_kind_fallbacks,
        unknown_reserved_system_kind_histogram = ?report.unknown_reserved_system_kind_histogram,
        unknown_reserved_effect_kind_histogram = ?report.unknown_reserved_effect_kind_histogram,
        cumulative_unknown_reserved_system_kind_histogram =
            ?report.cumulative_unknown_reserved_system_kind_histogram,
        cumulative_unknown_reserved_effect_kind_histogram =
            ?report.cumulative_unknown_reserved_effect_kind_histogram,
        "store open completed"
    );

    let Some(observer) = config.open_report_observer.as_ref() else {
        return;
    };
    let observer = Arc::clone(observer);
    if catch_unwind(AssertUnwindSafe(|| observer(report))).is_err() {
        tracing::warn!(
            target: "batpak::open",
            "open report observer panicked; continuing with successful open"
        );
    }
}

fn highest_index_hlc(index: &StoreIndex) -> HlcPoint {
    index
        .all_entries()
        .into_iter()
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .max()
        .unwrap_or(HlcPoint::ORIGIN)
}

fn last_close_hlc(index: &StoreIndex) -> Result<HlcPoint, StoreError> {
    let mut close_points: Vec<_> = index
        .all_entries()
        .into_iter()
        .filter(|entry| entry.kind == EventKind::SYSTEM_CLOSE_COMPLETED)
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .collect();
    close_points.sort_by_key(|point| point.global_sequence);

    let mut latest = HlcPoint::ORIGIN;
    for close_hlc in close_points {
        if close_hlc < latest {
            return Err(StoreError::InvariantViolation {
                reason: format!(
                    "SYSTEM_CLOSE_COMPLETED HLC regressed in log order: previous {:?}, later {:?}",
                    latest, close_hlc
                ),
            });
        }
        latest = close_hlc;
    }

    Ok(latest)
}

fn lifecycle_open_candidate(
    runtime: &config::ValidatedStoreConfig,
    max_recovered_hlc: HlcPoint,
    last_close_hlc: HlcPoint,
) -> Result<HlcPoint, StoreError> {
    let now_ms = match config::wall_ms_from_timestamp_us(runtime.now_us()) {
        Ok(now_ms) => now_ms,
        Err(StoreError::InvalidClock { .. }) => 0,
        Err(error) => return Err(error),
    };
    Ok(max_recovered_hlc.max(last_close_hlc).max(HlcPoint {
        wall_ms: now_ms,
        global_sequence: max_recovered_hlc.global_sequence,
    }))
}

fn validate_bootstrap_hlc(
    open_hlc: HlcPoint,
    max_recovered_hlc: HlcPoint,
    last_close_hlc: HlcPoint,
) -> Result<(), StoreError> {
    if open_hlc < max_recovered_hlc || open_hlc < last_close_hlc {
        return Err(StoreError::InvariantViolation {
            reason: format!(
                "open_hlc {:?} must be >= max_recovered_hlc {:?} and last_close_hlc {:?}",
                open_hlc, max_recovered_hlc, last_close_hlc
            ),
        });
    }
    Ok(())
}

fn bootstrap_open_hlc(
    runtime: &config::ValidatedStoreConfig,
    index: &StoreIndex,
) -> Result<HlcPoint, StoreError> {
    let max_recovered_hlc = highest_index_hlc(index);
    let last_close_hlc = last_close_hlc(index)?;
    let open_hlc = lifecycle_open_candidate(runtime, max_recovered_hlc, last_close_hlc)?;
    validate_bootstrap_hlc(open_hlc, max_recovered_hlc, last_close_hlc)?;
    Ok(open_hlc)
}

fn timestamp_us_for_hlc(point: HlcPoint) -> Result<i64, StoreError> {
    let timestamp_us =
        point
            .wall_ms
            .checked_mul(1000)
            .ok_or_else(|| StoreError::InvariantViolation {
                reason: format!("open_hlc wall_ms {} overflows timestamp_us", point.wall_ms),
            })?;
    i64::try_from(timestamp_us).map_err(|_| StoreError::InvariantViolation {
        reason: format!(
            "open_hlc wall_ms {} exceeds i64 timestamp_us range",
            point.wall_ms
        ),
    })
}

fn append_open_completed_event(
    store: &Store<Open>,
    report: &OpenIndexReport,
    open_candidate: HlcPoint,
) -> Result<HlcPoint, StoreError> {
    let coord = Coordinate::new("batpak:store", "batpak:lifecycle")?;
    let submission = AppendSubmission::with_options(
        AppendOptions::default().with_idempotency(crate::id::generate_v7_id()),
    );
    submission.validate_route(store)?;
    submission.validate_idempotency(store)?;
    let event = submission.build_event(
        report,
        EventKind::SYSTEM_OPEN_COMPLETED,
        timestamp_us_for_hlc(open_candidate)?,
    )?;

    let (tx, rx) = flume::bounded(1);
    let command = submission.into_command(coord, EventKind::SYSTEM_OPEN_COMPLETED, event, tx);
    store
        .writer_handle()?
        .tx
        .send(command)
        .map_err(|_| StoreError::WriterCrashed)?;
    let receipt = recv_writer_reply(&rx)?;
    let open_hlc = store
        .index
        .get_by_id(receipt.event_id)
        .map(|entry| HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        })
        .ok_or_else(|| StoreError::InvariantViolation {
            reason: format!(
                "SYSTEM_OPEN_COMPLETED receipt {:032x} was not visible in the rebuilt index",
                receipt.event_id
            ),
        })?;
    validate_bootstrap_hlc(open_hlc, open_candidate, last_close_hlc(&store.index)?)?;
    Ok(open_hlc)
}

impl Store<Open> {
    /// Open a store at the given config's data directory. Creates the directory if absent.
    /// Uses `NoCache` for projection (no external cache backend).
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock.
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NoCache))
    }

    /// Open a store with the built-in file-backed projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the cache directory cannot be created,
    /// or any error from [`Store::open_with_cache`].
    pub fn open_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open a store with a custom projection cache backend.
    /// Use [`NativeCache`] for file-backed cache-accelerated `project()` calls.
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock.
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
            cumulative_reserved_kind_fallbacks,
            store_lock,
        } = open_components(config, StoreLockMode::Mutable)?;

        let open_candidate = bootstrap_open_hlc(&runtime, &index)?;
        let subscribers = Arc::new(SubscriberList::new());
        let reactor_subscribers = Arc::new(ReactorSubscriberList::new());
        let writer = WriterHandle::spawn(
            &config,
            &runtime,
            &index,
            &subscribers,
            &reactor_subscribers,
            &reader,
        )?;
        let watermark_handle = writer.watermark_handle();
        let projection_registry = ProjectionRegistry::new(watermark_handle.clone());

        let store = Self {
            index,
            reader,
            cache,
            writer: Some(writer),
            watermark_handle,
            projection_registry,
            lifecycle_gate: Mutex::new(()),
            config,
            runtime,
            should_shutdown_on_drop: true,
            open_report: Some(open_report.clone()),
            cumulative_reserved_kind_fallbacks,
            _state: std::marker::PhantomData,
            _store_lock: store_lock,
        };

        emit_open_report_observability(&store.config, &open_report);
        let open_hlc = append_open_completed_event(&store, &open_report, open_candidate)?;
        store.watermark_handle.lock().reset_to_bootstrap(open_hlc);

        Ok(store)
    }

    /// Build a producer-side outbox for staged batch submission.
    pub fn outbox(&self) -> Outbox<'_> {
        Outbox::new(self, None)
    }

    /// Begin a public visibility fence. Only one fence may be active at a time.
    ///
    /// # Errors
    /// Returns an error if another public visibility fence is already active or
    /// if the writer cannot acknowledge the new fence.
    pub fn begin_visibility_fence(&self) -> Result<VisibilityFence<'_>, StoreError> {
        let token = self.index.begin_visibility_fence()?;
        let (tx, rx) = flume::bounded(1);
        let send_result = self
            .writer_handle()?
            .tx
            .send(WriterCommand::BeginVisibilityFence { token, respond: tx });
        if send_result.is_err() {
            let _ = self.index.cancel_visibility_fence(token);
            return Err(StoreError::WriterCrashed);
        }
        recv_writer_reply(&rx)?;
        Ok(VisibilityFence::new(self, token))
    }

    /// Snapshot the current writer mailbox pressure.
    pub fn writer_pressure(&self) -> WriterPressure {
        let writer = self.writer_ref();
        WriterPressure {
            queue_len: writer.tx.len(),
            capacity: self.config.writer.channel_capacity,
        }
    }

    /// Nonblocking root-cause append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the append for background execution.
    pub fn submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(coord, kind, payload, AppendSubmission::root())
    }

    /// Nonblocking reaction append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the reaction append for background execution.
    pub fn submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction(correlation_id, causation_id),
        )
    }

    /// Nonblocking batch append submission.
    ///
    /// Every item's coordinate is revalidated synchronously at this entry so
    /// that invalid coordinates surface to the caller rather than being
    /// deferred to the writer thread. Each item's serialized payload is also
    /// checked against `single_append_max_bytes` (G1): a single oversized
    /// item is rejected even when the batch-total cap would have allowed it.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCoordinate`] if any item's coordinate
    /// fails validation, [`StoreError::BatchItemTooLarge`] if any item's
    /// serialized payload exceeds `single_append_max_bytes`, or any enqueue
    /// or writer error surfaced while staging the batch for background
    /// execution.
    pub fn submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<BatchAppendTicket, StoreError> {
        self.ensure_no_active_public_fence()?;
        let per_item_cap = self.config.single_append_max_bytes as usize;
        for (i, item) in items.iter().enumerate() {
            if let Err(err) = item.coord().validate() {
                return Err(StoreError::InvalidCoordinate {
                    index: Some(i),
                    reason: format!("{err}"),
                });
            }
            let size = item.payload_bytes().len();
            if size > per_item_cap {
                return Err(StoreError::BatchItemTooLarge {
                    index: i,
                    size,
                    limit: per_item_cap,
                });
            }
        }
        self.submit_batch_with_fence_impl(items, None)
    }

    /// Attempt a root-cause submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit(coord, kind, payload)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a reaction submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a batch submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any enqueue or writer error surfaced when the operation
    /// proceeds past the soft-pressure gate.
    pub fn try_submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<crate::outcome::Outcome<BatchAppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate_batch() {
            return Ok(outcome);
        }
        self.submit_batch(items).map(crate::outcome::Outcome::ok)
    }

    /// WRITE: append a new root-cause event.
    /// correlation_id defaults to event_id (self-correlated). causation_id = None.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append",
            entity = coord.entity(),
            scope = coord.scope(),
            event_kind = kind.type_id()
        );
        self.submit(coord, kind, payload)?.wait()
    }

    /// WRITE: persist a gate denial as a normal per-entity chain event.
    ///
    /// # Errors
    /// Returns any serialization or writer error surfaced by the underlying
    /// append path.
    // justifies: Store::append_denial matches the substrate contract locked in this turn and mirrors the user-requested denial append surface; splitting it would add an extra request object without simplifying src/store/mod.rs.
    #[allow(clippy::too_many_arguments)]
    pub fn append_denial<Ctx>(
        &self,
        coord: &Coordinate,
        proposed_kind: EventKind,
        gate_set: &GateSet<Ctx>,
        failing: &Denial,
        proposed_content_hash: Option<[u8; 32]>,
        pipeline_id: Option<String>,
        options: AppendOptions,
    ) -> Result<DenialReceipt, StoreError> {
        let payload =
            gate_set.trace_denial(failing, proposed_kind, proposed_content_hash, pipeline_id);
        let receipt =
            self.append_with_options(coord, EventKind::SYSTEM_DENIAL, &payload, options)?;
        Ok(DenialReceipt {
            event_id: receipt.event_id,
            sequence: receipt.sequence,
            disk_pos: receipt.disk_pos,
            content_hash: receipt.content_hash,
            key_id: receipt.key_id,
            signature: receipt.signature,
            extensions: receipt.extensions,
        })
    }

    /// WRITE: append a reaction (caused by another event).
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_reaction",
            entity = coord.entity(),
            scope = coord.scope(),
            correlation_id = format_args!("{correlation_id:032x}"),
            causation_id = format_args!("{causation_id:032x}")
        );
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)?
            .wait()
    }

    /// WRITE: atomic batch append of multiple events.
    /// All events are committed together or none are visible.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if a specific item fails validation,
    /// encoding, marker writing, or publish preparation. Returns
    /// `StoreError::BatchSyncFailed` if the batch reaches the final durability
    /// boundary and segment sync fails before publish.
    pub fn append_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        self.append_batch_with_options(items, AppendOptions::default())
    }

    /// WRITE: atomic batch append with a batch-level append option set.
    ///
    /// Only [`AppendOptions::gate`] is honored at the batch level. The gate
    /// waits on the last event in the batch, which covers earlier events
    /// because batch HLCs and watermarks are monotonic.
    ///
    /// # Errors
    /// Returns any batch append error surfaced by [`Store::append_batch`].
    /// Returns [`StoreError::WaitTimeout`] or [`StoreError::WriterCrashed`] if
    /// the optional batch-level gate is not satisfied after the batch commits.
    pub fn append_batch_with_options(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
        opts: AppendOptions,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        debug_assert!(
            items.iter().all(|item| item.options().gate.is_none()),
            "BatchAppendItem per-item DurabilityGate is ignored; pass the gate to append_batch_with_options instead"
        );
        let gate = opts.gate;
        let receipts = self.submit_batch(items)?.wait()?;
        if let (Some(gate), Some(receipt)) = (gate, receipts.last()) {
            self.wait_for_gate(receipt, gate)?;
        }
        Ok(receipts)
    }

    /// WRITE: atomic batch append of reaction events.
    /// All events share the same correlation_id from the triggering event.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if a specific item fails validation,
    /// encoding, marker writing, or publish preparation. Returns
    /// `StoreError::BatchSyncFailed` if the batch reaches the final durability
    /// boundary and segment sync fails before publish.
    pub fn append_reaction_batch(
        &self,
        correlation_id: u128,
        causation_id: u128,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // Set correlation_id and causation_id on all items.
        let items: Vec<_> = items
            .into_iter()
            .map(|item| {
                let mut options = item.options();
                options.correlation_id = Some(correlation_id);
                // Only set causation_id if not already explicitly set.
                if item.causation().uses_options_fallback() {
                    options.causation_id = Some(causation_id);
                }
                item.with_options(options)
            })
            .collect();
        self.append_batch(items)
    }

    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe_lossy(&self, region: &Region) -> Subscription {
        // justifies: INV-TYPESTATE-OPEN-HAS-WRITER; Store<Open> typestate guarantees writer presence at
        // construction (see Store::open_with_cache in src/store/lifecycle.rs — it fails the open
        // instead of yielding Store<Open> if the writer cannot be spawned).
        // The expect here documents an invariant, it does not recover from
        // one: observing None means the store is mid-drop and every public
        // path through Store<Open> is already invalid.
        let rx = self
            .writer_ref()
            .subscribers
            .subscribe_with_region(self.config.broadcast_capacity, region.clone());
        Subscription::new(rx, region.clone())
    }

    /// Crate-private accessor that encodes the `Store<Open>` typestate
    /// invariant: an `Open` store always holds a writer handle.
    ///
    /// Panics if the invariant is violated — which only happens when a
    /// `Store<Open>` has been partially moved out of during drop, a context
    /// in which every public method is already unreachable.
    // justifies: INV-TYPESTATE-OPEN-HAS-WRITER and src/store/lifecycle.rs make this a typestate construction guarantee, not contingent runtime input.
    #[allow(clippy::expect_used)]
    pub(crate) fn writer_ref(&self) -> &WriterHandle {
        self.writer
            .as_ref()
            .expect("invariant: Store<Open> is constructed with a writer handle")
    }

    /// REACT: spawn a background thread running the subscribe→react→append loop.
    /// Returns a JoinHandle. The thread runs until the store is dropped (subscription closes).
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the background thread cannot be spawned.
    pub fn react_loop<R>(
        self: &Arc<Self>,
        region: &Region,
        reactor: R,
    ) -> Result<std::thread::JoinHandle<()>, StoreError>
    where
        R: crate::event::sourcing::Reactive<serde_json::Value> + Send + 'static,
    {
        let store = Arc::clone(self);
        let region = region.clone();
        let sub = self
            .writer_ref()
            .reactor_subscribers
            .subscribe(self.config.broadcast_capacity);
        std::thread::Builder::new()
            .name("batpak-reactor".into())
            .spawn(move || {
                while let Ok(envelope) = sub.recv() {
                    let notif = envelope.notification;
                    if !region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind)
                    {
                        continue;
                    }
                    for (coord, kind, payload) in reactor.react(&envelope.stored.event) {
                        if let Err(e) = store.append_reaction(
                            &coord,
                            kind,
                            &payload,
                            notif.correlation_id,
                            notif.event_id,
                        ) {
                            tracing::warn!("react_loop: failed to append reaction: {e}");
                        }
                    }
                }
            })
            .map_err(StoreError::Io)
    }

    /// WATCH: reactive projection subscription. Returns a `ProjectionWatcher`
    /// that re-projects `T` when new events arrive for `entity`.
    ///
    /// Internally subscribes to entity events, then re-projects on each notification.
    /// The watcher is pull-based: the caller drives the loop via
    /// [`ProjectionWatcher::recv`], which returns
    /// `Result<(u64, Option<T>), WatcherError>` — see the method docs for the
    /// full three-way return taxonomy (materialized state, empty fold, store
    /// closed, or watcher closure after the lossy/prunable subscription is
    /// dropped). The returned generation is persisted honestly: redundant
    /// wakeups for an already-delivered generation are suppressed, but an
    /// append that advances the entity generation can still yield the same
    /// folded state if `T::relevant_event_kinds()` filters it out.
    ///
    /// Requires `Arc<Store>` because the watcher outlives the borrow.
    pub fn watch_projection<T>(
        self: &Arc<Self>,
        entity: &str,
        freshness: Freshness,
    ) -> ProjectionWatcher<T>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    {
        let baseline_generation = self.entity_generation(entity).unwrap_or(0);
        let sub = self.subscribe_lossy(&Region::entity(entity));
        let post_subscribe_generation = self.entity_generation(entity).unwrap_or(0);
        let store = Arc::clone(self);
        let entity_owned = entity.to_owned();
        ProjectionWatcher::new(
            sub,
            store,
            entity_owned,
            freshness,
            baseline_generation,
            generation_advanced_after_subscribe(baseline_generation, post_subscribe_generation),
        )
    }

    /// WRITE: append with CAS, idempotency, custom correlation/causation.
    /// CAS and idempotency checks execute inside the writer thread under
    /// the entity lock — no TOCTOU race between check and commit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::SequenceMismatch` if the expected sequence does not match.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_with_options(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        let gate = opts.gate;
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_with_options",
            entity = coord.entity(),
            scope = coord.scope(),
            has_cas = opts.expected_sequence.is_some(),
            has_idempotency = opts.idempotency_key.is_some()
        );
        let receipt = self
            .submit_prepared(coord, kind, payload, AppendSubmission::with_options(opts))?
            .wait()?;
        if let Some(gate) = gate {
            self.wait_for_gate(&receipt, gate)?;
        }
        Ok(receipt)
    }

    /// WRITE: apply a typestate transition — kind is read from `P::KIND`.
    ///
    /// Per FREEZE-7 the transition's event kind is structurally derived from
    /// the payload type parameter, so this API cannot be called with a
    /// mismatched payload/kind pair.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn apply_transition<
        From: crate::typestate::transition::StateMarker,
        To: crate::typestate::transition::StateMarker,
        P: EventPayload,
    >(
        &self,
        coord: &Coordinate,
        transition: crate::typestate::transition::Transition<From, To, P>,
    ) -> Result<AppendReceipt, StoreError> {
        let payload = transition.into_payload();
        self.append(coord, P::KIND, &payload)
    }

    /// WRITE (typed): append a root-cause event — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendReceipt, StoreError> {
        self.append(coord, T::KIND, payload)
    }

    /// WRITE (typed): append with options — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed_with_options<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_with_options(coord, T::KIND, payload, opts)
    }

    /// WRITE (typed): nonblocking submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendTicket, StoreError> {
        self.submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): attempt submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): append a reaction — kind derived from `T::KIND`.
    ///
    /// `correlation_id` and `causation_id` are still supplied explicitly;
    /// only the `kind` becomes implicit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): nonblocking reaction submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): attempt reaction submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// LIFECYCLE
    ///
    /// # Errors
    /// Returns `StoreError::Io` if flushing the active segment to disk fails.
    pub fn sync(&self) -> Result<(), StoreError> {
        lifecycle::sync(self)
    }

    /// Block until the durable frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `durable_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_durable(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_durable(point, timeout)
    }

    /// Block until the applied frontier reaches `point` or `timeout` elapses.
    ///
    /// `applied_hlc` is the minimum applied HLC across registered projections,
    /// so a single lagging projection can keep this wait blocked.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `applied_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_applied(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_applied(point, timeout)
    }

    /// Block until the visible frontier reaches `point` or `timeout` elapses.
    ///
    /// # Errors
    /// Returns [`StoreError::WaitTimeout`] if `visible_hlc` does not reach
    /// `point` before `timeout`. Returns [`StoreError::WriterCrashed`] if the
    /// writer panicked while the caller was waiting.
    pub fn wait_for_visible(
        &self,
        point: HlcPoint,
        timeout: std::time::Duration,
    ) -> Result<(), StoreError> {
        self.watermark_handle.wait_for_visible(point, timeout)
    }

    /// Snapshot the current index to a destination directory.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        lifecycle::snapshot(self, dest)
    }

    /// Compact: merge sealed segments, optionally filtering events.
    /// The active (currently-written) segment is never touched.
    ///
    /// # F6 / FREEZE-4 swap contract
    ///
    /// The in-memory index is rebuilt off-side from the post-merge segment
    /// layout and then published as a single atomic swap under an exclusive
    /// lock (see `StoreIndex::replace_contents_from_fresh`). Reader-facing
    /// methods (`query`, `stream`, `cursor_guaranteed` polls, etc.) take a
    /// read guard on the same lock, so a concurrent reader observes either
    /// the pre-compact index or the post-compact index — never a cleared or
    /// partially rebuilt view.
    ///
    /// Failure modes are surfaced through the returned
    /// [`segment::CompactionResult`]:
    ///
    /// * [`segment::CompactionOutcome::Performed`] — the segment merge
    ///   happened and the live index has been swapped for the fresh one.
    /// * [`segment::CompactionOutcome::Skipped`] — the sealed-segment count
    ///   was below `min_segments`; no disk or index work was done.
    /// * [`segment::CompactionOutcome::Failed`] — the off-side rebuild
    ///   aborted before the swap point; the live index has not been
    ///   mutated, and the pending-compaction marker preserves a coherent
    ///   reopen path until cleanup completes.
    ///
    /// Appends that arrive during compaction are safe (they go to the active
    /// segment which is not compacted). `sync()` is called before and after
    /// the segment merge so the off-side rebuild sees a quiescent on-disk
    /// state; for maximum safety, avoid high-throughput appends during
    /// compaction.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if reading, writing, or removing segment
    /// files fails. A rebuild failure is NOT an error — it is reported via
    /// `CompactionOutcome::Failed`.
    pub fn compact(
        &self,
        config: &CompactionConfig,
    ) -> Result<crate::store::segment::CompactionResult, StoreError> {
        lifecycle::compact(self, config).map(|(result, _report)| result)
    }

    /// Same as [`Store::compact`], plus a deterministic structural
    /// [`CompactionReportBody`] for evidence.
    ///
    /// # Errors
    /// Same error paths as [`Store::compact`].
    pub fn compact_with_report(
        &self,
        config: &CompactionConfig,
    ) -> Result<
        (
            crate::store::segment::CompactionResult,
            CompactionReportBody,
        ),
        StoreError,
    > {
        lifecycle::compact(self, config)
    }

    /// LIFECYCLE: flush pending writes and shut down the writer thread cleanly.
    ///
    /// # Errors
    /// Returns `StoreError::WriterCrashed` if the writer thread has already exited unexpectedly.
    pub fn close(self) -> Result<Closed, StoreError> {
        lifecycle::close(self)
    }
}

impl Store<ReadOnly> {
    /// Open the store without starting a writer thread.
    ///
    /// # Errors
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NoCache))
    }

    /// Open the store in read-only mode with the built-in projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the native cache cannot be
    /// opened, or any error returned by [`Store::open_read_only_with_cache`].
    pub fn open_read_only_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open the store in read-only mode with a custom projection cache backend.
    ///
    /// # Errors
    /// Returns [`StoreError::StoreLocked`] if another live store handle already
    /// owns the directory lock. Read-only opens are also exclusive under the
    /// current store-ownership contract.
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
            cumulative_reserved_kind_fallbacks,
            store_lock,
        } = open_components(config, StoreLockMode::ReadOnly)?;

        let open_hlc = bootstrap_open_hlc(&runtime, &index)?;
        let watermark_handle = WatermarkState::bootstrap_handle(open_hlc);
        let projection_registry = ProjectionRegistry::new(watermark_handle.clone());
        let store = Self {
            index,
            reader,
            cache,
            writer: None,
            watermark_handle,
            projection_registry,
            lifecycle_gate: Mutex::new(()),
            config,
            runtime,
            should_shutdown_on_drop: false,
            open_report: Some(open_report.clone()),
            cumulative_reserved_kind_fallbacks,
            _state: std::marker::PhantomData,
            _store_lock: store_lock,
        };

        emit_open_report_observability(&store.config, &open_report);

        Ok(store)
    }
}

impl<State> Store<State> {
    /// READ: get a single event by ID.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading from disk fails.
    pub fn get(&self, event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let entry = self
            .index
            .get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: fetch a single event by ID with the payload left as raw
    /// MessagePack bytes. Mirrors [`get`](Self::get) but skips the
    /// JSON-decode step, suitable for the `RawMsgpackInput` lane of a
    /// multi-event reactor.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading
    /// from disk fails.
    pub fn get_raw(&self, event_id: u128) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        let entry = self
            .index
            .get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry_raw(&entry.disk_pos)
    }

    /// Verify an append receipt against the store's signing-key registry and
    /// current index state.
    #[must_use]
    pub fn verify_append_receipt(&self, receipt: &AppendReceipt) -> bool {
        let Some(entry) = self.index.get_by_id(receipt.event_id) else {
            return false;
        };
        self.runtime.signing_registry.verify_append_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// Verify a persisted denial receipt against the store's signing-key
    /// registry and current index state.
    #[must_use]
    pub fn verify_denial_receipt(&self, receipt: &DenialReceipt) -> bool {
        let Some(entry) = self.index.get_by_id(receipt.event_id) else {
            return false;
        };
        self.runtime.signing_registry.verify_denial_receipt(
            receipt,
            &entry.coord,
            entry.kind,
            entry.hash_chain.prev_hash,
        )
    }

    /// READ: query by Region.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors.
    pub fn walk_ancestors(
        &self,
        event_id: u128,
        limit: usize,
    ) -> Vec<StoredEvent<serde_json::Value>> {
        ancestry::walk_ancestors(self, event_id, limit)
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    ///
    /// # Errors
    /// Returns any replay, deserialization, cache, or disk-read error surfaced
    /// while reconstructing the projection state.
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project(self, entity, freshness)
    }

    /// Return the current per-entity generation if the entity exists.
    ///
    /// Generations advance monotonically on every insert for that entity.
    /// When entity-group overlays are disabled, this falls back to the entity
    /// stream length so callers still get a stable monotonic skip token.
    pub fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.index.entity_generation(entity)
    }

    /// Project only when the entity changed since `last_seen_generation`.
    ///
    /// Returns `Ok(None)` when no change is observed. Otherwise returns the
    /// generation at which the returned state was materialized together with
    /// the freshly projected state. The returned generation is honest: a
    /// cache-hit path returns the generation at which the cache was
    /// stamped, a replay path returns the generation sampled before replay
    /// started. Callers who persist this generation as a watermark (e.g.
    /// [`ProjectionWatcher`]) will not silently consume a relevant append
    /// against stale state (F5). To preserve that property, this API treats
    /// [`Freshness::MaybeStale`] the same as [`Freshness::Consistent`].
    ///
    /// # Errors
    /// Returns any error surfaced by [`Store::project`] when the entity has
    /// changed and the projection must be rebuilt.
    pub fn project_if_changed<T>(
        &self,
        entity: &str,
        last_seen_generation: u64,
        freshness: &Freshness,
    ) -> Result<Option<(u64, Option<T>)>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_if_changed(self, entity, last_seen_generation, freshness)
    }

    /// CONVENIENCE: sugar over index.stream() for exact entity match.
    #[must_use]
    pub fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.index.stream(entity)
    }

    /// READ: query all events in the given scope.
    #[must_use]
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }

    /// READ: query all events of the given event kind across all entities and scopes.
    #[must_use]
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// READ (typed): query all events whose kind matches `T::KIND`.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`.
    #[must_use]
    pub fn by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry> {
        self.by_fact(T::KIND)
    }

    /// CURSOR: pull-based, ordered delivery from the in-memory index.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`. This cursor is
    /// process-local only: it does not persist its position, so restart-time
    /// at-least-once semantics require the checkpoint-bound cursor worker
    /// surface rather than this constructor.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }

    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        lifecycle::stats(self)
    }

    /// Return detailed diagnostic information about the store's internal state.
    pub fn diagnostics(&self) -> StoreDiagnostics {
        lifecycle::diagnostics(self)
    }

    /// Deterministic store resource evidence over stable [`StoreDiagnostics`] facts.
    ///
    /// Canonical identity excludes raw paths (uses [`store_data_dir_identity_hash`]),
    /// free-form envelope diagnostics, and timestamps outside the structured cold-start
    /// report. Metadata fields on the returned envelope are unset by default.
    ///
    /// # Errors
    /// Canonical body encoding failure while computing `body_hash`.
    pub fn store_resource_evidence_report(
        &self,
    ) -> Result<StoreResourceEvidenceReport, StoreResourceReportError> {
        store_resource_evidence_report_from_diagnostics(&lifecycle::diagnostics(self))
    }

    /// Return the current operator-facing frontier view.
    pub fn frontier(&self) -> FrontierView {
        self.watermark_handle.lock().snapshot_view()
    }

    /// Return a coherent clone of the internal frontier watermarks.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_watermark_snapshot(&self) -> WatermarkSnapshot {
        self.watermark_handle.lock().snapshot()
    }

    /// Register a projection ID in the applied-frontier registry.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_register_projection(&self, projection_id: &str) {
        self.projection_registry.register(projection_id.to_owned());
    }

    /// Register the same projection ID used by `project::<T>()` for `entity`.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_register_projection_for<T: 'static>(&self, entity: &str) {
        self.projection_registry
            .register(ProjectionRegistry::id_for_type::<T>(entity));
    }

    /// Report projection progress directly for focused frontier tests.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_notify_projection_applied(&self, projection_id: &str, point: HlcPoint) {
        self.projection_registry
            .notify_applied(projection_id.to_owned(), point);
    }

    /// Remove a projection ID from the applied-frontier registry.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_unregister_projection(&self, projection_id: &str) {
        self.projection_registry.unregister(projection_id);
    }

    /// Wake frontier waiters without advancing a watermark.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_notify_watermark_waiters(&self) {
        self.watermark_handle.dangerous_notify_all();
    }
}

/// Safety net: if Store is dropped without calling close(), send a best-effort
/// Shutdown to the writer thread and wait briefly for it to drain pending events.
/// close(self) is still the preferred explicit path for guaranteed clean shutdown.
impl<State> Drop for Store<State> {
    fn drop(&mut self) {
        if !self.should_shutdown_on_drop {
            return;
        }
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        tracing::warn!(
            "Store dropped without explicit close(); only a bounded best-effort drain will run"
        );
        let (tx, rx) = flume::bounded(1);
        if writer
            .tx
            .send(WriterCommand::Shutdown { respond: tx })
            .is_ok()
        {
            // Wait up to 100ms for the writer to drain pending events.
            // This prevents data loss when Store is dropped without close().
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn next_active_segment_id_is_one_past_latest_existing_segment() {
        let dir = TempDir::new().expect("temp dir");
        std::fs::write(dir.path().join(segment::segment_filename(1)), b"").expect("segment 1");
        std::fs::write(dir.path().join(segment::segment_filename(7)), b"").expect("segment 7");

        assert_eq!(
            next_active_segment_id(dir.path()),
            8,
            "PROPERTY: reader active segment must be one past the highest existing segment so the last sealed segment remains mmap-eligible"
        );
    }

    #[test]
    fn highest_index_hlc_reports_non_origin_point_for_appended_entry() {
        let dir = TempDir::new().expect("temp dir");
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let coord = Coordinate::new("entity:highest-hlc", "scope:test").expect("coord");
        let receipt = store
            .append(
                &coord,
                EventKind::custom(0xF, 0x77),
                &serde_json::json!({"x": 1}),
            )
            .expect("append");

        let point = highest_index_hlc(&store.index);

        assert_eq!(
            point.global_sequence, receipt.sequence,
            "PROPERTY: highest_index_hlc must observe the committed entry's global sequence"
        );
        assert!(
            point > HlcPoint::ORIGIN,
            "PROPERTY: highest_index_hlc must not collapse a non-empty index to origin/default"
        );

        store.close().expect("close");
    }

    #[test]
    fn generation_advanced_after_subscribe_is_strictly_forward() {
        assert!(
            !generation_advanced_after_subscribe(7, 7),
            "PROPERTY: equal baseline/post-subscribe generations must not trigger an initial watcher catch-up"
        );
        assert!(
            generation_advanced_after_subscribe(7, 8),
            "PROPERTY: a post-subscribe generation above baseline must trigger the initial watcher catch-up"
        );
        assert!(
            !generation_advanced_after_subscribe(8, 7),
            "PROPERTY: older post-subscribe observations must never trigger an initial watcher catch-up"
        );
    }
}
