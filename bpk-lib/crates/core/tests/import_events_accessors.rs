//! Store import API accessor proofs.
//!
//! PROVES: ImportSelector/ImportOptions accessors reflect configuration —
//! region_ref, after_global_sequence, chunk_size, and source_namespace return
//! the values they were built with.
//! CATCHES: accessor mutants that return a default/constant instead of the
//! configured value (region_ref -> leaked default Region; chunk_size -> 1).
//! SEEDED: pure in-memory ImportSelector/ImportOptions values; no store.

use batpak::store::{ImportOptions, ImportSelector, SourceNamespace};
use batpak_testkit::prelude::*;

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
        o.source_namespace().as_str(),
        "ns-xyz",
        "source_namespace() must reflect the configured namespace"
    );
    Ok(())
}

/// PROVES: `SourceNamespace::new` rejects the empty namespace and round-trips a
/// non-empty one through `as_str`/`Display`, with serde transparent to the inner
/// string. CATCHES: a constructor that accepts an empty namespace, a wrapper
/// that mangles the value, or a non-transparent serde form.
#[test]
fn source_namespace_validates_and_is_transparent() {
    assert!(
        SourceNamespace::new("").is_err(),
        "empty source namespace must be rejected"
    );
    let ns = SourceNamespace::new("ns-transparent").expect("non-empty namespace");
    assert_eq!(ns.as_str(), "ns-transparent");
    assert_eq!(ns.to_string(), "ns-transparent");
    let json = serde_json::to_string(&ns).expect("serialize");
    assert_eq!(json, "\"ns-transparent\"", "serde form must be transparent");
    let decoded: SourceNamespace = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(decoded, ns);
}
