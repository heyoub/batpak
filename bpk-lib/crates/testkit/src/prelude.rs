//! Broad integration-test imports.
//!
//! Public `batpak::prelude` is intentionally beginner-oriented for 0.8. The
//! integration suite still exercises advanced batteries directly; keeping that
//! broad surface here prevents test import churn from shaping user-facing API.
//!
//! Every entry below is a genuine `pub use` re-export (this is a real lib
//! crate, not a `#[path]`-included module), so `unused_imports` does not fire
//! and no allow attribute is required.

pub use batpak::artifact::{
    ArtifactEnvelopeFinding, ArtifactEnvelopeIdentity, ArtifactHash, ArtifactVerificationReport,
    AttestationRef, CanonicalArtifactEnvelope, SignatureEnvelope, SignatureRef,
    ARTIFACT_ENVELOPE_FRAMING_VERSION,
};
pub use batpak::coordinate::{
    ClockRange, Coordinate, CoordinateError, DagPosition, EventCategory, KindFilter, Region,
    RegionFilterError,
};
pub use batpak::event::sourcing::{MultiDispatchError, MultiReactive, Reactive, TypedReactive};
pub use batpak::event::{
    revalidate_event_payload_registry, revalidate_upcast_chain_registry,
    validate_event_payload_registry, validate_upcast_chain_registry, DecodeSource, DecodeTyped,
    Event, EventHeader, EventKind, EventKindError, EventPayload, EventPayloadKindCollision,
    EventPayloadRegistryError, EventPayloadValidation, EventSourced, HashChain,
    IncompleteUpcastChain, JsonValueInput, ProjectionEvent, ProjectionInput, ProjectionPayload,
    ProjectionStateContract, RawMsgpackInput, ReplayLane, StateExtent, StateExtentCost,
    StoredEvent, TypedDecodeError, UpcastChainRegistryError,
};
pub use batpak::guard::{Denial, Gate, GateSet, Receipt};
pub use batpak::id::{CausationId, CorrelationId, EventId};
pub use batpak::outcome::{ErrorKind, Outcome, OutcomeError};
pub use batpak::pipeline::{CommitMetadata, Committed, Pipeline, Proposal};
pub use batpak::schema::{
    compare_schema_snapshot, SchemaChangeClass, SchemaSnapshot, SchemaSnapshotEvidenceReport,
    SchemaSnapshotFinding, SchemaSnapshotReportBody, SchemaSnapshotReportError,
    SCHEMA_SNAPSHOT_REPORT_SCHEMA_VERSION,
};
pub use batpak::store::delivery::cursor::{
    CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle,
};
pub use batpak::store::delivery::subscription::{Subscription, SubscriptionOps};
pub use batpak::store::{
    AppendOptions, AppendPositionHint, AppendReceipt, AppendTicket, BatchAppendItem,
    BatchAppendTicket, BatchConfig, CausationRef, ChainWalkEvidenceReport, ChainWalkFinding,
    ChainWalkMode, ChainWalkReportBody, ChainWalkReportError, ChainWalkRequest, ChainWalkStartRef,
    Closed, CompactionConfig, CompactionStrategy, Cursor, DurabilityGate, Freshness, HlcPoint,
    IndexConfig, IndexTopology, LossPrecision, NoCache, Notification, Open,
    ProjectionRunCacheStatus, ProjectionRunCheckpointRef, ProjectionRunEvidenceReport,
    ProjectionRunFinding, ProjectionRunFreshnessStatus, ProjectionRunFrontierKind,
    ProjectionRunInputFrontier, ProjectionRunOutputHash, ProjectionRunReplayMode,
    ProjectionRunReportBody, ProjectionRunReportError, ProjectionRunRequestedFreshness,
    ProjectionSourceRef, ReactionBatch, ReactorCanal, ReactorConfig, ReactorError, ReadOnly,
    ReadWalkDroppedCount, ReadWalkEvidenceReport, ReadWalkFinding, ReadWalkFreshnessIntent,
    ReadWalkFrontierKind, ReadWalkInputFrontier, ReadWalkProofRef, ReadWalkProofRefs,
    ReadWalkReplayMode, ReadWalkReportBody, ReadWalkReportError, ReadWalkRequest,
    ReadWalkSourceRef, ReceiptExtensionKey, ReceiptExtensionNamespace, ReceiptExtensionValue,
    ReceiptVerification, ReceiptVerificationError, RestartPolicy, SigningDowngradeBody,
    SigningDowngradeReason, SigningExtensionNamespace, SnapshotEvidenceHash,
    SnapshotEvidenceReport, SnapshotFenceTokenRef, SnapshotFileKind, SnapshotFinding,
    SnapshotReportBody, SnapshotWatermarkRef, Store, StoreConfig, StoreError,
    StoreResourceEvidenceReport, StoreResourceFrontierBody, StoreResourceHash,
    StoreResourceReportBody, StoreResourceReportError, StoreResourceRestartPolicyShape,
    SubscriberDeliveryState, SubscriberFrontierEvidenceReport, SubscriberFrontierFinding,
    SubscriberFrontierReportBody, SubscriberFrontierReportError, SubscriberFrontierRequest,
    SubscriberFrontierSource, SyncConfig, SyncMode, TypedReactorHandle, WatermarkKind,
    WriterConfig, WriterPressure, CHAIN_WALK_REPORT_SCHEMA_VERSION,
    PROJECTION_RUN_REPORT_SCHEMA_VERSION, READ_WALK_REPORT_SCHEMA_VERSION,
    SIGNING_DOWNGRADE_SCHEMA_VERSION, SNAPSHOT_EVIDENCE_REPORT_SCHEMA_VERSION,
    STORE_RESOURCE_REPORT_SCHEMA_VERSION, SUBSCRIBER_FRONTIER_REPORT_SCHEMA_VERSION,
};
pub use batpak_macros::{EventPayload, EventSourced, MultiEventReactor};
