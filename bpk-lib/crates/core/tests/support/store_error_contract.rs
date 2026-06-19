//! Shared `StoreError` contract-table fixtures for the split
//! `store_error_contract*` integration harnesses.
//!
//! Included via `#[path = "support/store_error_contract.rs"]` by every
//! `store_error_contract*` test binary. The harness was split out of a single
//! 521-line file (over the 500-line cap); the classification table, the `Case`
//! shape, and the per-variant case builders live here so each test binary stays
//! small while sharing one source of truth for the contract. Every split binary
//! routes through [`contract_table`] (filtering to its family), so every
//! per-family builder is consumed in every binary — no `dead_code` surface and
//! no escape hatch required (see ADR-0012).

use batpak::coordinate::CoordinateError;
use batpak::event::{EventPayloadKindCollision, EventPayloadRegistryError};
use batpak::store::{
    CheckpointIdError, HiddenRangesCorruption, HlcPoint, ProfileInvalidKind, StoreError,
    StoreInvariant, StoreLockMode, WatermarkKind,
};
use std::error::Error as _;
use std::io;
use std::path::PathBuf;

/// Downstream handling class a `StoreError` variant must keep stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandlingClass {
    Domain,
    RetryableOperational,
    FailClosedOperational,
}

/// One row of the `StoreError` contract table: an error value plus the
/// handling class, source forwarding, and `Display` fields it must preserve.
pub struct Case {
    pub name: &'static str,
    pub error: StoreError,
    pub class: HandlingClass,
    pub source_needle: Option<&'static str>,
    pub display_needles: &'static [&'static str],
}

/// Map a `StoreError` to its required downstream handling class.
///
/// Every public variant is matched by an EXPLICIT arm (no catch-all that swallows
/// a real variant), so adding a new `StoreError` variant makes this `match` fail
/// to compile until it is given a reviewed classification. `StoreError` is
/// `#[non_exhaustive]`, so the Rust compiler still requires a trailing wildcard:
/// it is genuinely unreachable for any variant defined today and exists only to
/// satisfy the `#[non_exhaustive]` obligation. The wildcard panics so that a
/// future variant added in the defining crate (which would compile here without
/// touching this file) still fails loudly the first time it is classified rather
/// than silently borrowing a neighbour's class.
pub fn classify(error: &StoreError) -> HandlingClass {
    match error {
        StoreError::Io(_)
        | StoreError::CacheFailed(_)
        | StoreError::CheckpointWriteFailed { .. }
        | StoreError::IdempotencyOverflowFailClosed { .. }
        | StoreError::WaitTimeout { .. } => HandlingClass::RetryableOperational,
        StoreError::StoreLocked { .. }
        | StoreError::Coordinate(_)
        | StoreError::CheckpointId(_)
        | StoreError::EventPayloadRegistry(_)
        | StoreError::InvalidPayloadVersion { .. }
        | StoreError::NotFound(_)
        | StoreError::SequenceMismatch { .. }
        | StoreError::Configuration(_)
        | StoreError::IdempotencyRequired
        | StoreError::VisibilityFenceActive
        | StoreError::VisibilityFenceNotActive
        | StoreError::VisibilityFenceCancelled
        | StoreError::IdempotencyPartialBatch { .. }
        | StoreError::RangeMalformed { .. }
        | StoreError::InvalidCoordinate { .. }
        | StoreError::ReservedKind { .. }
        | StoreError::InvalidCausation { .. }
        | StoreError::InvalidCommitMetadata { .. }
        | StoreError::CoordinateNulByte
        | StoreError::CoordinatePathTraversal
        | StoreError::CoordinateControlChar
        | StoreError::BatchItemTooLarge { .. }
        | StoreError::EntityClockOverflow { .. }
        | StoreError::InvalidClock { .. } => HandlingClass::Domain,
        StoreError::BatchFailed { source, .. } | StoreError::BatchSyncFailed { source, .. } => {
            classify(source.as_ref())
        }
        StoreError::Serialization(_)
        | StoreError::CrcMismatch { .. }
        | StoreError::CorruptSegment { .. }
        | StoreError::PlatformProfileInvalid { .. }
        | StoreError::PlatformProfileMismatch { .. }
        | StoreError::PlatformAdmissionFailed { .. }
        | StoreError::WriterCrashed
        | StoreError::SequenceGateViolation { .. }
        | StoreError::CorruptFrame { .. }
        | StoreError::SegmentTooManyEntries { .. }
        | StoreError::DataDirMalformed { .. }
        | StoreError::AncestryCorrupt { .. }
        | StoreError::IdempotencyFutureVersion { .. }
        | StoreError::MmapFutureVersion { .. }
        | StoreError::HiddenRangesCorrupt { .. }
        | StoreError::CursorCheckpointCorrupt { .. }
        | StoreError::CursorCheckpointRegionMismatch { .. }
        | StoreError::InvariantViolation { .. } => HandlingClass::FailClosedOperational,
        #[cfg(feature = "dangerous-test-hooks")]
        StoreError::FaultInjected(_) => HandlingClass::FailClosedOperational,
        // `StoreError` is `#[non_exhaustive]`; this wildcard is unreachable for
        // every variant that exists today (all are matched explicitly above) and
        // is required only to satisfy the compiler. Any variant later added in
        // the defining crate trips this panic the first time it is classified.
        _ => panic!(
            "STORE_ERROR CONTRACT TABLE OUT OF DATE: add an explicit handling class for {error:?}"
        ),
    }
}

/// Assert one contract-table row: classification stability, every `Display`
/// needle, and the source-forwarding contract. Shared by every split binary so
/// the assertions stay byte-identical across families.
pub fn assert_case_contract(case: &Case) {
    let display = case.error.to_string();
    let source = case.error.source().map(std::string::ToString::to_string);

    assert_eq!(
        classify(&case.error),
        case.class,
        "STORE_ERROR CLASSIFICATION DRIFT: {} should stay {:?}, got {:?}. display={display}",
        case.name,
        case.class,
        classify(&case.error)
    );

    for needle in case.display_needles {
        assert!(
            display.contains(needle),
            "STORE_ERROR DISPLAY DRIFT: {} must include {:?}.\n\
             display={display}",
            case.name,
            needle
        );
    }

    match case.source_needle {
        Some(needle) => {
            let Some(source) = source.as_deref() else {
                panic!(
                    "STORE_ERROR SOURCE DRIFT: {} should expose an underlying source error",
                    case.name
                );
            };
            assert!(
                source.contains(needle),
                "STORE_ERROR SOURCE DRIFT: {} should expose {:?}, got {:?}",
                case.name,
                needle,
                source
            );
        }
        None => {
            assert!(
                source.is_none(),
                "STORE_ERROR SOURCE DRIFT: {} should not expose an underlying source, got {:?}",
                case.name,
                source
            );
        }
    }
}

/// The full `StoreError` contract table across every handling-class family.
/// Every split binary routes through this and filters to its own family, so all
/// per-family builders below are consumed in every binary (no dead-code surface)
/// while the table stays a single source of truth.
pub fn contract_table() -> Vec<Case> {
    let mut cases = domain_cases();
    cases.extend(retryable_operational_cases());
    cases.extend(fail_closed_operational_cases());
    cases
}

/// Contract rows whose required handling class is [`HandlingClass::Domain`]:
/// caller-fault rejections that callers can correct and retry.
pub fn domain_cases() -> Vec<Case> {
    vec![
        Case {
            name: "store_locked",
            error: StoreError::StoreLocked {
                path: PathBuf::from("fixtures/locked-store"),
                mode: StoreLockMode::ReadOnly,
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["fixtures/locked-store", "read-only", "locked"],
        },
        Case {
            name: "not_found",
            error: StoreError::NotFound(0xDEAD),
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["dead", "not found"],
        },
        Case {
            name: "sequence_mismatch",
            error: StoreError::SequenceMismatch {
                entity: "user:1".into(),
                expected: 5,
                actual: 3,
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["user:1", "5", "3", "CAS failed"],
        },
        Case {
            name: "configuration",
            error: StoreError::Configuration("single_append_max_bytes must be > 0".into()),
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["invalid config", "single_append_max_bytes"],
        },
        Case {
            name: "invalid_coordinate",
            error: StoreError::InvalidCoordinate {
                index: Some(4),
                reason: "entity cannot be empty".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &[
                "batch item 4",
                "entity cannot be empty",
                "invalid coordinate",
            ],
        },
        Case {
            name: "reserved_kind_single",
            error: StoreError::ReservedKind {
                index: None,
                kind: 0x0006,
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["reserved kind 0x0006", "public surface"],
        },
        Case {
            name: "reserved_kind_batch_item",
            error: StoreError::ReservedKind {
                index: Some(3),
                kind: 0xD001,
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["reserved kind 0xD001", "batch item 3"],
        },
        Case {
            name: "batch_item_too_large",
            error: StoreError::BatchItemTooLarge {
                index: 1,
                size: 4097,
                limit: 2048,
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["batch item 1", "4097", "2048"],
        },
        Case {
            name: "invalid_clock",
            error: StoreError::InvalidClock {
                timestamp_us: -17,
                reason: "timestamp_us must be >= 0 microseconds since Unix epoch".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["-17", "invalid", "timestamp_us"],
        },
        Case {
            name: "coordinate",
            error: StoreError::Coordinate(CoordinateError::EmptyEntity),
            class: HandlingClass::Domain,
            source_needle: Some("entity cannot be empty"),
            display_needles: &["coordinate error", "entity cannot be empty"],
        },
        Case {
            name: "checkpoint_id",
            error: StoreError::CheckpointId(CheckpointIdError::ForbiddenSeparator),
            class: HandlingClass::Domain,
            source_needle: Some("forbidden identity-separator"),
            display_needles: &["checkpoint id error", "identity-separator"],
        },
        Case {
            name: "event_payload_registry",
            error: StoreError::EventPayloadRegistry(EventPayloadRegistryError::new(vec![
                EventPayloadKindCollision {
                    category: 0xF,
                    type_id: 0x0FE,
                    first_type_name: "crate_a::Widget",
                    second_type_name: "crate_b::Gadget",
                },
            ])),
            class: HandlingClass::Domain,
            source_needle: Some("duplicate kind assignment"),
            display_needles: &[
                "EventPayload registry",
                "duplicate kind assignment",
                "category=0xF",
                "crate_a::Widget",
            ],
        },
        Case {
            name: "invalid_payload_version",
            error: StoreError::InvalidPayloadVersion { kind: 0x1234 },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &[
                "0x1234",
                "PAYLOAD_VERSION 0",
                "reserved legacy/untyped sentinel",
            ],
        },
        Case {
            name: "idempotency_required",
            error: StoreError::IdempotencyRequired,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["group commit", "requires an idempotency key"],
        },
        Case {
            name: "idempotency_partial_batch",
            error: StoreError::IdempotencyPartialBatch {
                reason: "items 0,2 carry idempotency keys but item 1 does not".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &[
                "batch rejected",
                "items 0,2 carry idempotency keys but item 1 does not",
            ],
        },
        Case {
            name: "visibility_fence_active",
            error: StoreError::VisibilityFenceActive,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["visibility fence is already active"],
        },
        Case {
            name: "visibility_fence_not_active",
            error: StoreError::VisibilityFenceNotActive,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["no matching visibility fence is currently active"],
        },
        Case {
            name: "visibility_fence_cancelled",
            error: StoreError::VisibilityFenceCancelled,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["visibility fence was cancelled before publish"],
        },
        Case {
            name: "range_malformed",
            error: StoreError::RangeMalformed { start: 9, end: 4 },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["malformed range", "start=9", "end=4", "start must be < end"],
        },
        Case {
            name: "invalid_causation",
            error: StoreError::InvalidCausation {
                prior_idx: 5,
                item_index: 3,
                reason: "PriorItem must reference an earlier item".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &[
                "invalid causation",
                "item 3",
                "prior 5",
                "PriorItem must reference an earlier item",
            ],
        },
        Case {
            name: "invalid_commit_metadata",
            error: StoreError::InvalidCommitMetadata {
                reason: "commit metadata key must not be empty".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &[
                "invalid commit metadata",
                "commit metadata key must not be empty",
            ],
        },
        Case {
            name: "entity_clock_overflow",
            error: StoreError::EntityClockOverflow {
                entity: "user:42".into(),
            },
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["entity user:42", "u32::MAX", "further appends rejected"],
        },
        Case {
            name: "coordinate_nul_byte",
            error: StoreError::CoordinateNulByte,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["coordinate component contains forbidden NUL byte"],
        },
        Case {
            name: "coordinate_path_traversal",
            error: StoreError::CoordinatePathTraversal,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["forbidden path-traversal substring"],
        },
        Case {
            name: "coordinate_control_char",
            error: StoreError::CoordinateControlChar,
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["forbidden ASCII control character"],
        },
    ]
}

/// Contract rows whose required handling class is
/// [`HandlingClass::RetryableOperational`]: transient operational faults a
/// caller may safely retry, including the batch wrappers that inherit their
/// inner error's class.
pub fn retryable_operational_cases() -> Vec<Case> {
    vec![
        Case {
            name: "io",
            error: StoreError::Io(io::Error::new(io::ErrorKind::TimedOut, "disk timed out")),
            class: HandlingClass::RetryableOperational,
            source_needle: Some("disk timed out"),
            display_needles: &["IO error", "disk timed out"],
        },
        Case {
            name: "cache_failed",
            error: StoreError::CacheFailed(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                "cache timed out",
            ))),
            class: HandlingClass::RetryableOperational,
            source_needle: Some("cache timed out"),
            display_needles: &["cache error", "cache timed out"],
        },
        Case {
            name: "wait_timeout",
            error: StoreError::WaitTimeout {
                watermark: WatermarkKind::Durable,
                target: HlcPoint {
                    wall_ms: 123,
                    global_sequence: 4,
                },
                waited_ms: 250,
            },
            class: HandlingClass::RetryableOperational,
            source_needle: None,
            display_needles: &["Durable", "123", "4", "250ms", "timed out"],
        },
        Case {
            name: "batch_failed_wraps_inner_contract",
            error: StoreError::BatchFailed {
                item_index: 2,
                source: Box::new(StoreError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "flush timed out",
                ))),
            },
            class: HandlingClass::RetryableOperational,
            source_needle: Some("IO error: flush timed out"),
            display_needles: &["batch failed at item 2", "flush timed out"],
        },
        Case {
            name: "batch_sync_failed_wraps_inner_contract",
            error: StoreError::BatchSyncFailed {
                item_count: 3,
                source: Box::new(StoreError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "segment fsync timed out",
                ))),
            },
            class: HandlingClass::RetryableOperational,
            source_needle: Some("IO error: segment fsync timed out"),
            display_needles: &[
                "batch sync failed after writing 3 items",
                "segment fsync timed out",
            ],
        },
        Case {
            name: "checkpoint_write_failed",
            error: StoreError::CheckpointWriteFailed {
                id: "reactor-a".into(),
                source: io::Error::new(io::ErrorKind::TimedOut, "checkpoint fsync timed out"),
            },
            class: HandlingClass::RetryableOperational,
            source_needle: Some("checkpoint fsync timed out"),
            display_needles: &["reactor-a", "write failed", "checkpoint fsync timed out"],
        },
        Case {
            // Soft-cap backpressure: not corruption. A later retry can succeed
            // once out-of-window keys age out and free durable capacity, so this
            // is operationally retryable rather than a fail-closed halt.
            name: "idempotency_overflow_fail_closed",
            error: StoreError::IdempotencyOverflowFailClosed {
                len: 1024,
                max_keys: 1024,
            },
            class: HandlingClass::RetryableOperational,
            source_needle: None,
            display_needles: &[
                "durable idempotency store at soft cap",
                "1024/1024",
                "overflow policy fail-closed",
            ],
        },
    ]
}

/// Contract rows whose required handling class is
/// [`HandlingClass::FailClosedOperational`]: corruption and invariant
/// violations that must halt rather than retry.
pub fn fail_closed_operational_cases() -> Vec<Case> {
    vec![
        Case {
            name: "sequence_gate_violation",
            error: StoreError::SequenceGateViolation {
                operation: "publish_then_broadcast_unfenced",
                requested: 7,
                allocated: 5,
                visible: 4,
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "publish_then_broadcast_unfenced",
                "publish(7)",
                "allocated=5",
                "visible=4",
            ],
        },
        Case {
            name: "serialization",
            error: StoreError::Serialization(Box::new(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad msgpack",
            ))),
            class: HandlingClass::FailClosedOperational,
            source_needle: Some("bad msgpack"),
            display_needles: &["serialization error", "bad msgpack"],
        },
        Case {
            name: "crc_mismatch",
            error: StoreError::CrcMismatch {
                segment_id: 7,
                offset: 42,
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["CRC mismatch", "7", "42"],
        },
        Case {
            name: "corrupt_segment",
            error: StoreError::CorruptSegment {
                segment_id: 8,
                detail: "unsupported segment version: 99".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["corrupt segment", "8", "unsupported segment version"],
        },
        Case {
            name: "corrupt_frame",
            error: StoreError::CorruptFrame {
                segment_id: 9,
                offset: 128,
                reason: "bad crc region".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["corrupt frame", "9", "128", "bad crc region"],
        },
        Case {
            name: "hidden_ranges_corrupt",
            error: StoreError::HiddenRangesCorrupt {
                path: PathBuf::from("fixtures/hidden-ranges.json"),
                kind: HiddenRangesCorruption::ReadFailed(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF",
                )),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: Some("unexpected EOF"),
            display_needles: &["fixtures/hidden-ranges.json", "unexpected EOF", "corrupt"],
        },
        Case {
            name: "invariant_violation",
            error: StoreError::InvariantViolation {
                kind: StoreInvariant::CloseHlcRegression {
                    previous: HlcPoint {
                        wall_ms: 2,
                        global_sequence: 2,
                    },
                    later: HlcPoint {
                        wall_ms: 1,
                        global_sequence: 3,
                    },
                },
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["invariant violation", "HLC regressed"],
        },
        Case {
            name: "platform_profile_invalid",
            error: StoreError::PlatformProfileInvalid {
                path: PathBuf::from("fixtures/platform/bad.profile"),
                kind: ProfileInvalidKind::UnsupportedSchemaVersion {
                    observed: 2,
                    expected: 1,
                },
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["fixtures/platform/bad.profile", "invalid", "schema_version"],
        },
        Case {
            name: "platform_profile_mismatch",
            error: StoreError::PlatformProfileMismatch {
                path: PathBuf::from("fixtures/platform/linux_basic.profile"),
                reason: "expected AtomicNoFollow, observed BestEffortCheckThenOpen".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "fixtures/platform/linux_basic.profile",
                "does not match",
                "AtomicNoFollow",
            ],
        },
        Case {
            name: "platform_admission_failed",
            error: StoreError::PlatformAdmissionFailed {
                capability: "sealed segment mmap",
                reason: "mmap evidence Unknown is not admissible".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["sealed segment mmap", "admission failed", "Unknown"],
        },
        Case {
            name: "cursor_checkpoint_corrupt",
            error: StoreError::CursorCheckpointCorrupt {
                path: PathBuf::from("fixtures/cursors/reactor-a.ckpt"),
                reason: "invalid msgpack".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "fixtures/cursors/reactor-a.ckpt",
                "invalid msgpack",
                "corrupt",
            ],
        },
        Case {
            name: "cursor_checkpoint_region_mismatch",
            error: StoreError::CursorCheckpointRegionMismatch {
                path: PathBuf::from("fixtures/cursors/reactor-a.ckpt"),
                stored: Some("entity_prefix=user:".into()),
                expected: "entity_prefix=order:".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "fixtures/cursors/reactor-a.ckpt",
                "entity_prefix=user:",
                "entity_prefix=order:",
                "belongs to region",
            ],
        },
        Case {
            name: "writer_crashed",
            error: StoreError::WriterCrashed,
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["writer thread crashed"],
        },
        Case {
            name: "segment_too_many_entries",
            error: StoreError::SegmentTooManyEntries {
                segment_id: 12,
                count: 5_000_000_000,
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["segment 12", "5000000000 entries", "u32 footer capacity"],
        },
        Case {
            name: "data_dir_malformed",
            error: StoreError::DataDirMalformed {
                path: PathBuf::from("fixtures/data/not-a-segment.txt"),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "data directory contains unexpected file",
                "fixtures/data/not-a-segment.txt",
            ],
        },
        Case {
            name: "ancestry_corrupt",
            error: StoreError::AncestryCorrupt { cycle_at: 0xBEEF },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["ancestry walk detected a cycle", "beef"],
        },
        Case {
            name: "idempotency_future_version",
            error: StoreError::IdempotencyFutureVersion {
                stored: 9,
                current: 2,
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "durable idempotency store on disk is version 9",
                "understands at most version 2",
                "upgrade the reader",
            ],
        },
        Case {
            name: "mmap_future_version",
            error: StoreError::MmapFutureVersion {
                found: 9,
                supported: 5,
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "mmap index on disk is version 9",
                "understands at most version 5",
                "refusing to rebuild from scan",
                "upgrade the reader",
            ],
        },
        #[cfg(feature = "dangerous-test-hooks")]
        Case {
            name: "fault_injected",
            error: StoreError::FaultInjected("simulated fsync failure at segment boundary".into()),
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &[
                "fault injected",
                "simulated fsync failure at segment boundary",
            ],
        },
    ]
}
