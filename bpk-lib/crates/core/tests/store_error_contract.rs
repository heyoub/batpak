//! PROVES: domain-class `StoreError` variants and the coordinate/IO conversion
//! routes preserve handling class, source forwarding, and `Display` fields.
//! CATCHES: drift where a domain-fault `StoreError` arm or a `From` conversion
//! drops identity, source, or handling-class stability without a table update.
//! SEEDED: deterministic contract table (domain family + conversion routes).

use batpak_testkit::store_error_contract as store_error_support;

use batpak::coordinate::{Coordinate, CoordinateError};
use batpak::event::{EventPayloadKindCollision, EventPayloadRegistryError};
use batpak::store::{
    CheckpointIdError, HiddenRangesCorruption, HlcPoint, ProfileInvalidKind, StoreError,
    StoreInvariant, StoreLockMode, WatermarkKind,
};
use std::io;
use std::path::PathBuf;
use store_error_support::*;

/// One representative value of EVERY public `StoreError` variant.
///
/// This is the spine of the table-completeness guard
/// ([`every_store_error_variant_has_a_contract_case`]): the `match` below is
/// written without a catch-all over the non-wrapper variants, so adding a new
/// `StoreError` variant forces this list to be extended (in the defining crate
/// it would compile, but here every existing variant is enumerated explicitly
/// and a forgotten one is caught by the discriminant assertion). `StoreError`
/// is `#[non_exhaustive]`, so a wildcard arm is required and full compile-time
/// enforcement is not reachable from this downstream test crate; the residual
/// gap is exactly "a variant added in `bpk-lib/crates/core` itself but in no
/// other place" — see the module note on `classify` for the matching argument.
fn one_of_every_variant() -> Vec<StoreError> {
    let representatives = vec![
        StoreError::Io(io::Error::new(io::ErrorKind::TimedOut, "io")),
        StoreError::StoreLocked {
            path: PathBuf::from("p"),
            mode: StoreLockMode::ReadOnly,
        },
        StoreError::Coordinate(CoordinateError::EmptyEntity),
        StoreError::CheckpointId(CheckpointIdError::Empty),
        StoreError::Serialization(Box::new(io::Error::new(io::ErrorKind::InvalidData, "ser"))),
        StoreError::CrcMismatch {
            segment_id: 1,
            offset: 2,
        },
        StoreError::CorruptSegment {
            segment_id: 1,
            detail: "d".into(),
        },
        StoreError::NotFound(batpak::id::EventId::from(1u128)),
        StoreError::SequenceMismatch {
            entity: "e".into(),
            expected: 1,
            actual: 2,
        },
        StoreError::WriterCrashed,
        StoreError::WaitTimeout {
            watermark: WatermarkKind::Durable,
            target: HlcPoint {
                wall_ms: 1,
                global_sequence: 1,
            },
            waited_ms: 1,
        },
        StoreError::CacheFailed(Box::new(io::Error::other("c"))),
        StoreError::SequenceGateViolation {
            operation: "op",
            requested: 1,
            allocated: 1,
            visible: 1,
        },
        StoreError::Configuration("c".into()),
        StoreError::PlatformProfileInvalid {
            path: PathBuf::from("p"),
            kind: ProfileInvalidKind::UnsupportedSchemaVersion {
                observed: 2,
                expected: 1,
            },
        },
        StoreError::PlatformProfileMismatch {
            path: PathBuf::from("p"),
            reason: "r".into(),
        },
        StoreError::PlatformAdmissionFailed {
            capability: "cap",
            reason: "r".into(),
        },
        StoreError::EventPayloadRegistry(EventPayloadRegistryError::new(vec![
            EventPayloadKindCollision {
                category: 0x1,
                type_id: 0x002,
                first_type_name: "a",
                second_type_name: "b",
            },
        ])),
        StoreError::IdempotencyRequired,
        StoreError::VisibilityFenceActive,
        StoreError::VisibilityFenceNotActive,
        StoreError::VisibilityFenceCancelled,
        StoreError::BatchFailed {
            item_index: 0,
            source: Box::new(StoreError::WriterCrashed),
        },
        StoreError::BatchSyncFailed {
            item_count: 1,
            source: Box::new(StoreError::WriterCrashed),
        },
        StoreError::IdempotencyPartialBatch { reason: "r".into() },
        StoreError::IdempotencyFutureVersion {
            stored: 2,
            current: 1,
        },
        StoreError::MmapFutureVersion {
            found: 6,
            supported: 5,
        },
        StoreError::CheckpointFutureVersion {
            found: 7,
            supported: 6,
        },
        StoreError::HiddenRangesFutureVersion {
            path: PathBuf::from("p"),
            found: 2,
            supported: 1,
        },
        StoreError::IdempotencyOverflowFailClosed {
            len: 1,
            max_keys: 1,
        },
        StoreError::InvalidPayloadVersion { kind: 1 },
        StoreError::CorruptFrame {
            segment_id: 1,
            offset: 2,
            reason: "r".into(),
        },
        StoreError::SegmentTooManyEntries {
            segment_id: 1,
            count: 1,
        },
        StoreError::InternerExhausted { count: 1 },
        StoreError::DataDirMalformed {
            path: PathBuf::from("p"),
        },
        StoreError::AncestryCorrupt {
            cycle_at: batpak::id::EventId::from(1u128),
        },
        StoreError::RangeMalformed { start: 2, end: 1 },
        StoreError::InvalidCoordinate {
            index: None,
            reason: "r".into(),
        },
        StoreError::ReservedKind {
            index: None,
            kind: 1,
        },
        StoreError::InvalidCausation {
            prior_idx: 1,
            item_index: 0,
            reason: "r".into(),
        },
        StoreError::InvalidCommitMetadata { reason: "r".into() },
        StoreError::CoordinateNulByte,
        StoreError::CoordinatePathTraversal,
        StoreError::CoordinateControlChar,
        StoreError::HiddenRangesCorrupt {
            path: PathBuf::from("p"),
            kind: HiddenRangesCorruption::ReadFailed(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "eof",
            )),
        },
        StoreError::BatchItemTooLarge {
            index: 0,
            size: 2,
            limit: 1,
        },
        StoreError::EntityClockOverflow { entity: "e".into() },
        StoreError::InvalidClock {
            timestamp_us: -1,
            reason: "r".into(),
        },
        StoreError::CheckpointWriteFailed {
            id: "i".into(),
            source: io::Error::other("w"),
        },
        StoreError::CursorCheckpointCorrupt {
            path: PathBuf::from("p"),
            reason: "r".into(),
        },
        StoreError::CursorCheckpointRegionMismatch {
            path: PathBuf::from("p"),
            stored: None,
            expected: "e".into(),
        },
        StoreError::InvariantViolation {
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
        #[cfg(feature = "dangerous-test-hooks")]
        StoreError::FaultInjected("f".into()),
    ];
    // Guard against an explicit-arm match (below) silently going stale: each
    // representative must be one we can name. The match has no catch-all over
    // real variants, so a NEW variant added in this crate would force a compile
    // error here when this list is extended to include it.
    for error in &representatives {
        match error {
            StoreError::Io(_)
            | StoreError::StoreLocked { .. }
            | StoreError::Coordinate(_)
            | StoreError::CheckpointId(_)
            | StoreError::Serialization(_)
            | StoreError::CrcMismatch { .. }
            | StoreError::CorruptSegment { .. }
            | StoreError::NotFound(_)
            | StoreError::SequenceMismatch { .. }
            | StoreError::WriterCrashed
            | StoreError::WaitTimeout { .. }
            | StoreError::CacheFailed(_)
            | StoreError::SequenceGateViolation { .. }
            | StoreError::Configuration(_)
            | StoreError::PlatformProfileInvalid { .. }
            | StoreError::PlatformProfileMismatch { .. }
            | StoreError::PlatformAdmissionFailed { .. }
            | StoreError::EventPayloadRegistry(_)
            | StoreError::IdempotencyRequired
            | StoreError::VisibilityFenceActive
            | StoreError::VisibilityFenceNotActive
            | StoreError::VisibilityFenceCancelled
            | StoreError::BatchFailed { .. }
            | StoreError::BatchSyncFailed { .. }
            | StoreError::IdempotencyPartialBatch { .. }
            | StoreError::IdempotencyFutureVersion { .. }
            | StoreError::MmapFutureVersion { .. }
            | StoreError::CheckpointFutureVersion { .. }
            | StoreError::HiddenRangesFutureVersion { .. }
            | StoreError::IdempotencyOverflowFailClosed { .. }
            | StoreError::InvalidPayloadVersion { .. }
            | StoreError::CorruptFrame { .. }
            | StoreError::SegmentTooManyEntries { .. }
            | StoreError::InternerExhausted { .. }
            | StoreError::DataDirMalformed { .. }
            | StoreError::AncestryCorrupt { .. }
            | StoreError::RangeMalformed { .. }
            | StoreError::InvalidCoordinate { .. }
            | StoreError::ReservedKind { .. }
            | StoreError::InvalidCausation { .. }
            | StoreError::InvalidCommitMetadata { .. }
            | StoreError::CoordinateNulByte
            | StoreError::CoordinatePathTraversal
            | StoreError::CoordinateControlChar
            | StoreError::HiddenRangesCorrupt { .. }
            | StoreError::BatchItemTooLarge { .. }
            | StoreError::EntityClockOverflow { .. }
            | StoreError::InvalidClock { .. }
            | StoreError::CheckpointWriteFailed { .. }
            | StoreError::CursorCheckpointCorrupt { .. }
            | StoreError::CursorCheckpointRegionMismatch { .. }
            | StoreError::InvariantViolation { .. } => {}
            #[cfg(feature = "dangerous-test-hooks")]
            StoreError::FaultInjected(_) => {}
            // `StoreError` is `#[non_exhaustive]`: a wildcard is mandatory and a
            // future variant added in the defining crate cannot be compile-forced
            // into this list from a downstream test crate. The completeness
            // assertion below still catches it at runtime once it appears.
            _ => {}
        }
    }
    representatives
}

#[test]
fn store_error_contract_domain_family_stays_stable() {
    let cases: Vec<_> = contract_table()
        .into_iter()
        .filter(|case| case.class == HandlingClass::Domain)
        .collect();
    assert!(
        !cases.is_empty(),
        "STORE_ERROR CONTRACT DRIFT: expected Domain cases in contract_table()"
    );
    for case in &cases {
        assert_case_contract(case);
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
    assert!(
        matches!(routed, StoreError::Coordinate(_)),
        "COORDINATE ROUTING DRIFT: EmptyEntity should stay wrapped in StoreError::Coordinate"
    );
    let StoreError::Coordinate(inner) = routed else {
        unreachable!("matched StoreError::Coordinate above")
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
    assert!(
        matches!(routed, StoreError::Io(_)),
        "IO ROUTING DRIFT: std::io::Error should stay wrapped in StoreError::Io"
    );
    let StoreError::Io(source) = routed else {
        unreachable!("matched StoreError::Io above")
    };
    assert_eq!(source.kind(), io::ErrorKind::TimedOut);
}

/// Table-completeness guard: every public `StoreError` variant must have at
/// least one [`Case`](store_error_support::Case) row in `contract_table()`.
///
/// [`one_of_every_variant`] enumerates one representative of every variant; this
/// test asserts each representative's `std::mem::discriminant` is present among
/// the contract-table rows. A variant added to the enum AND to
/// `one_of_every_variant` but forgotten in `contract_table()` fails here, which
/// closes the historical gap where ~14 classified variants had no asserted row.
///
/// `StoreError` is `#[non_exhaustive]`, so this cannot be made fully
/// compile-time-exhaustive from a downstream test crate (a wildcard arm is
/// mandatory in every `match` over it). The residual gap is "a variant added in
/// `bpk-lib/crates/core` itself and added nowhere else"; `classify`'s panicking
/// wildcard is the matching runtime backstop for that case.
#[test]
fn every_store_error_variant_has_a_contract_case() {
    let table = contract_table();
    let covered: Vec<std::mem::Discriminant<StoreError>> = table
        .iter()
        .map(|case| std::mem::discriminant(&case.error))
        .collect();

    for variant in one_of_every_variant() {
        let want = std::mem::discriminant(&variant);
        assert!(
            covered.contains(&want),
            "STORE_ERROR CONTRACT TABLE INCOMPLETE: variant {variant:?} has no Case row in \
             contract_table(); add one to the matching family builder in \
             tests/support/store_error_contract.rs"
        );
    }
}
