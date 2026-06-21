//! Store import API accessor proofs.
//!
//! PROVES: ImportSelector/ImportOptions accessors reflect configuration —
//! region_ref, after_global_sequence, chunk_size, and source_namespace return
//! the values they were built with.
//! CATCHES: accessor mutants that return a default/constant instead of the
//! configured value (region_ref -> leaked default Region; chunk_size -> 1).
//! SEEDED: pure in-memory ImportSelector/ImportOptions values; no store.

mod support;
use batpak::store::{ImportOptions, ImportSelector};
use support::prelude::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn import_selector_accessors_reflect_configuration() -> TestResult {
    // Region is not PartialEq, so discriminate via its public `entity_prefix()`
    // accessor: a region-scoped selector must carry the DISTINCTIVE prefix, not
    // the `None` prefix of a default `Region`. This kills the
    // `region_ref -> Box::leak(Box::new(Default::default()))` mutant, whose
    // leaked default Region has `entity_prefix() == None`.
    let selector = ImportSelector::region(Region::entity("distinctive:region:prefix"));
    assert_eq!(
        selector.region_ref().entity_prefix(),
        Some("distinctive:region:prefix"),
        "region_ref() must reflect the configured region, not a leaked default"
    );

    assert_eq!(
        ImportSelector::after(42).after_global_sequence(),
        Some(42),
        "after(42) must expose the configured resume point"
    );
    assert_eq!(
        ImportSelector::all().after_global_sequence(),
        None,
        "all() carries no resume point"
    );
    Ok(())
}

#[test]
fn import_options_accessors_reflect_configuration() -> TestResult {
    let o = ImportOptions::new("ns-xyz")?.with_chunk_size(7);
    assert_eq!(
        o.chunk_size(),
        7,
        "chunk_size() must reflect the configured value, not the constant 1"
    );
    assert_eq!(
        o.source_namespace(),
        "ns-xyz",
        "source_namespace() must reflect the configured namespace"
    );
    Ok(())
}
