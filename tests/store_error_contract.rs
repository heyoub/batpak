// justifies: INV-TEST-PANIC-AS-ASSERTION; this contract-table harness uses panic! to make variant/source drift fail loudly and locally.
#![allow(clippy::panic)]
//! PROVES: representative `StoreError` variants preserve a stable downstream
//! handling contract: domain rejections stay distinct from retryable
//! operational failures and fail-closed operational failures, source-bearing
//! variants keep forwarding their underlying error, and user-facing `Display`
//! text keeps surfacing the fields callers need to diagnose the failure.
//! CATCHES: drift where a public `StoreError` arm stops exposing its key
//! identity in `Display`, silently drops an underlying source error, or moves
//! between downstream handling classes without an explicit table update.
//! SEEDED: not random; deterministic contract table.

use batpak::coordinate::{Coordinate, CoordinateError};
use batpak::store::{HlcPoint, StoreError, StoreLockMode, WatermarkKind};
use std::error::Error as _;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandlingClass {
    Domain,
    RetryableOperational,
    FailClosedOperational,
}

struct Case {
    name: &'static str,
    error: StoreError,
    class: HandlingClass,
    source_needle: Option<&'static str>,
    display_needles: &'static [&'static str],
}

fn classify(error: &StoreError) -> HandlingClass {
    match error {
        StoreError::Io(_)
        | StoreError::CacheFailed(_)
        | StoreError::CheckpointWriteFailed { .. }
        | StoreError::WaitTimeout { .. } => HandlingClass::RetryableOperational,
        StoreError::StoreLocked { .. }
        | StoreError::Coordinate(_)
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
        | StoreError::WriterCrashed
        | StoreError::SequenceGateViolation { .. }
        | StoreError::CorruptFrame { .. }
        | StoreError::SegmentTooManyEntries { .. }
        | StoreError::DataDirMalformed { .. }
        | StoreError::AncestryCorrupt { .. }
        | StoreError::HiddenRangesCorrupt { .. }
        | StoreError::CursorCheckpointCorrupt { .. }
        | StoreError::CursorCheckpointRegionMismatch { .. } => HandlingClass::FailClosedOperational,
        #[cfg(feature = "dangerous-test-hooks")]
        StoreError::FaultInjected(_) => HandlingClass::FailClosedOperational,
        _ => panic!(
            "STORE_ERROR CONTRACT TABLE OUT OF DATE: add an explicit handling class for {error:?}"
        ),
    }
}

#[test]
fn store_error_contract_table_stays_stable() {
    let cases = [
        Case {
            name: "io",
            error: StoreError::Io(io::Error::new(io::ErrorKind::TimedOut, "disk timed out")),
            class: HandlingClass::RetryableOperational,
            source_needle: Some("disk timed out"),
            display_needles: &["IO error", "disk timed out"],
        },
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
            name: "configuration",
            error: StoreError::Configuration("single_append_max_bytes must be > 0".into()),
            class: HandlingClass::Domain,
            source_needle: None,
            display_needles: &["invalid config", "single_append_max_bytes"],
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
                reason: "unexpected EOF".into(),
            },
            class: HandlingClass::FailClosedOperational,
            source_needle: None,
            display_needles: &["fixtures/hidden-ranges.json", "unexpected EOF", "corrupt"],
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
    ];

    for case in cases {
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
}

#[test]
fn coordinate_and_io_conversion_preserve_store_error_routing() {
    let hardening_cases = [
        (
            CoordinateError::NulByte,
            StoreError::CoordinateNulByte,
            "coordinate component contains forbidden NUL byte",
        ),
        (
            CoordinateError::ControlChar,
            StoreError::CoordinateControlChar,
            "coordinate component contains forbidden ASCII control character",
        ),
        (
            CoordinateError::PathTraversal,
            StoreError::CoordinatePathTraversal,
            "coordinate component contains forbidden path-traversal substring",
        ),
    ];

    for (coordinate_error, expected_store_error, expected_display) in hardening_cases {
        let actual = StoreError::from(coordinate_error.clone());
        assert!(
            std::mem::discriminant(&actual) == std::mem::discriminant(&expected_store_error),
            "COORDINATE ROUTING DRIFT: {coordinate_error:?} should route to {expected_store_error:?}, got {actual:?}"
        );
        assert_eq!(
            classify(&actual),
            HandlingClass::Domain,
            "COORDINATE ROUTING CLASS DRIFT: {:?} should stay a domain rejection",
            actual
        );
        assert!(
            actual.to_string().contains(expected_display),
            "COORDINATE ROUTING DISPLAY DRIFT: expected {:?} to contain {:?}",
            actual,
            expected_display
        );
    }

    let empty_entity = Coordinate::new("", "scope").expect_err("empty entity should be rejected");
    let routed = StoreError::from(empty_entity.clone());
    let StoreError::Coordinate(inner) = routed else {
        panic!(
            "COORDINATE ROUTING DRIFT: EmptyEntity should stay wrapped in StoreError::Coordinate"
        );
    };
    assert_eq!(
        inner, empty_entity,
        "COORDINATE ROUTING DRIFT: non-hardening coordinate errors should preserve the original payload"
    );
    assert_eq!(
        classify(&StoreError::Coordinate(inner)),
        HandlingClass::Domain,
        "COORDINATE ROUTING CLASS DRIFT: wrapped coordinate validation must stay a domain rejection"
    );

    let io_error = io::Error::new(io::ErrorKind::TimedOut, "fsync timed out");
    let routed = StoreError::from(io_error);
    let StoreError::Io(source) = routed else {
        panic!("IO ROUTING DRIFT: std::io::Error should stay wrapped in StoreError::Io");
    };
    assert_eq!(source.kind(), io::ErrorKind::TimedOut);
}
