// justifies: INV-TEST-PANIC-AS-ASSERTION; canonical patch-stability golden tests assert via panic and intentionally support explicit fixture regeneration.
#![allow(clippy::panic, clippy::print_stderr)]
//! Patch-stability tests for schema snapshot evidence body bytes.
//!
//! PROVES: INV-CANONICAL-PATCH-STABILITY
//! CATCHES: accidental field-order, serde-shape, or report-body identity drift
//! across patch releases.
//! SEEDED: deterministic golden body plus proptest-generated equal snapshots.

use batpak::schema::{compare_schema_snapshot, SchemaSnapshot, SchemaSnapshotReportBody};
use proptest::prelude::*;
use serde::Serialize;
use std::error::Error;

#[path = "common/proptest.rs"]
mod proptest_support;

type TestResult = Result<(), Box<dyn Error>>;

fn golden_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn check_or_update_golden(name: &str, actual_bytes: &[u8]) {
    let path = golden_dir().join(name);
    let actual_hex = hex_encode(actual_bytes);
    let updating = std::env::var("GOLDEN_UPDATE").as_deref() == Ok("I_KNOW_WHAT_IM_DOING");
    if updating {
        eprintln!("GOLDEN_UPDATE: regenerating golden file {}", path.display());
        std::fs::write(&path, &actual_hex)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
        return;
    }

    let expected_hex = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "golden file {} not found: {error}. Run \
             GOLDEN_UPDATE=I_KNOW_WHAT_IM_DOING cargo test -p batpak --test canonical_patch_stability",
            path.display()
        )
    });
    assert_eq!(
        actual_hex.trim(),
        expected_hex.trim(),
        "CANONICAL PATCH DRIFT: {name} no longer matches {}",
        path.display()
    );
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn body_hash_via_canonical(body: &(impl Serialize + Sized)) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = batpak::canonical::to_bytes(body)?;
    Ok(evidence_content_hash(&bytes))
}

fn evidence_content_hash(bytes: &[u8]) -> [u8; 32] {
    #[cfg(feature = "blake3")]
    {
        batpak::event::hash::compute_hash(bytes)
    }
    #[cfg(not(feature = "blake3"))]
    {
        let crc = crc32fast::hash(bytes).to_be_bytes();
        let mut out = [0_u8; 32];
        out[..4].copy_from_slice(&crc);
        out
    }
}

fn sample_report_body(
) -> Result<SchemaSnapshotReportBody, batpak::schema::SchemaSnapshotReportError> {
    let expected = SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x22; 32]);
    let observed = SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x23; 32]);
    Ok(compare_schema_snapshot(&expected, &observed)?.body)
}

proptest! {
    #![proptest_config(proptest_support::cfg(256))]

    #[test]
    fn schema_snapshot_report_body_canonical_bytes_are_patch_stable_for_equal_logical_inputs(
        stable_id in "[a-z][a-z0-9_.:-]{0,31}",
        schema_hash in any::<[u8; 32]>(),
        fixture_hash in any::<[u8; 32]>(),
    ) {
        let expected = SchemaSnapshot::from_hashes(stable_id, schema_hash, fixture_hash);
        let observed = expected.clone();

        let first = compare_schema_snapshot(&expected, &observed)?;
        let second = compare_schema_snapshot(&expected, &observed)?;
        let first_bytes = batpak::canonical::to_bytes(&first.body)?;
        let second_bytes = batpak::canonical::to_bytes(&second.body)?;
        let decoded: SchemaSnapshotReportBody = batpak::canonical::from_bytes(&first_bytes)?;

        prop_assert_eq!(&first.body, &second.body);
        prop_assert_eq!(&first_bytes, &second_bytes);
        prop_assert_eq!(decoded, first.body);
        prop_assert_eq!(first.body_hash, evidence_content_hash(&first_bytes));
    }
}

#[test]
fn schema_snapshot_report_body_hash_matches_generated_canonical_bytes() -> TestResult {
    let body = sample_report_body()?;
    let report_hash = body_hash_via_canonical(&body)?;
    let report = compare_schema_snapshot(
        &SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x22; 32]),
        &SchemaSnapshot::from_hashes("schema.snapshot.report.v1", [0x11; 32], [0x23; 32]),
    )?;

    assert_eq!(
        report.body_hash, report_hash,
        "PROPERTY: schema snapshot report body_hash must equal hash(canonical(body))"
    );
    Ok(())
}

#[test]
fn schema_snapshot_report_body_v1_golden_bytes_do_not_drift() -> TestResult {
    let body = sample_report_body()?;
    let bytes = batpak::canonical::to_bytes(&body)?;
    check_or_update_golden("schema_snapshot_report_body_v1.hex", &bytes);

    let decoded: SchemaSnapshotReportBody = batpak::canonical::from_bytes(&bytes)?;
    assert_eq!(
        decoded, body,
        "PROPERTY: schema snapshot report body v1 golden bytes must round-trip"
    );
    Ok(())
}
