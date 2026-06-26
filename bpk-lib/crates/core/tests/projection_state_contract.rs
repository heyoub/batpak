//! Public projection-state contract constructor coverage.
//!
//! These tests keep the constructor surface visible to the build-time public
//! item coverage gate while pinning the exact data those helpers produce.

use batpak::prelude::{ProjectionStateContract, StateExtent, StateExtentCost};

#[test]
fn bounded_contract_constructor_records_declared_fields() {
    let contract = ProjectionStateContract::bounded(
        "bounded-test-keyspace",
        7,
        "retain seven",
        "compact on checkpoint",
        "checkpoint every run",
    );

    let ProjectionStateContract::Bounded {
        key_space,
        max_cardinality,
        retention_policy,
        compaction_policy,
        checkpoint_policy,
    } = contract
    else {
        assert!(
            std::hint::black_box(false),
            "PROPERTY: bounded() must construct ProjectionStateContract::Bounded"
        );
        return;
    };

    assert_eq!(key_space, "bounded-test-keyspace");
    assert_eq!(max_cardinality, 7);
    assert_eq!(retention_policy, "retain seven");
    assert_eq!(compaction_policy, "compact on checkpoint");
    assert_eq!(checkpoint_policy, "checkpoint every run");
}

#[test]
fn state_extent_constructors_record_cardinality_and_cost() {
    let measured = StateExtent::cardinality(3, StateExtentCost::Incremental);
    assert_eq!(measured.cardinality, Some(3));
    assert_eq!(measured.cost, StateExtentCost::Incremental);

    let unavailable = StateExtent::unavailable();
    assert_eq!(unavailable.cardinality, None);
    assert_eq!(unavailable.cost, StateExtentCost::Unavailable);
}
