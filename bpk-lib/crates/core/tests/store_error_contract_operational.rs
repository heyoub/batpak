// justifies: INV-TEST-PANIC-AS-ASSERTION; this contract-table harness uses panic! to make variant/source drift fail loudly and locally.
#![allow(clippy::panic)]
//! PROVES: operational `StoreError` variants preserve their downstream handling
//! class (retryable vs fail-closed), source forwarding, and `Display` fields.
//! CATCHES: drift where a retryable or fail-closed `StoreError` arm drops
//! identity, source, or handling-class stability without an explicit table update.
//! SEEDED: deterministic contract table (retryable + fail-closed families).

#[path = "support/store_error_contract.rs"]
mod store_error_support;

use store_error_support::*;

#[test]
fn store_error_contract_retryable_operational_family_stays_stable() {
    let cases: Vec<_> = contract_table()
        .into_iter()
        .filter(|case| case.class == HandlingClass::RetryableOperational)
        .collect();
    assert!(
        !cases.is_empty(),
        "STORE_ERROR CONTRACT DRIFT: expected RetryableOperational cases in contract_table()"
    );
    for case in &cases {
        assert_case_contract(case);
    }
}

#[test]
fn store_error_contract_fail_closed_operational_family_stays_stable() {
    let cases: Vec<_> = contract_table()
        .into_iter()
        .filter(|case| case.class == HandlingClass::FailClosedOperational)
        .collect();
    assert!(
        !cases.is_empty(),
        "STORE_ERROR CONTRACT DRIFT: expected FailClosedOperational cases in contract_table()"
    );
    for case in &cases {
        assert_case_contract(case);
    }
}
