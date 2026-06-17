// justifies: INV-TEST-PANIC-AS-ASSERTION; this contract-table harness uses panic! to make variant/source drift fail loudly and locally.
#![allow(clippy::panic)]
//! PROVES: domain-class `StoreError` variants and the coordinate/IO conversion
//! routes preserve handling class, source forwarding, and `Display` fields.
//! CATCHES: drift where a domain-fault `StoreError` arm or a `From` conversion
//! drops identity, source, or handling-class stability without a table update.
//! SEEDED: deterministic contract table (domain family + conversion routes).

#[path = "support/store_error_contract.rs"]
mod store_error_support;

use batpak::coordinate::{Coordinate, CoordinateError};
use batpak::store::StoreError;
use std::io;
use store_error_support::*;

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
