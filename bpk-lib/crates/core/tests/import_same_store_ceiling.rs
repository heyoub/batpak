//! justifies: INV-IMPORT-NO-RUNAWAY
//!
//! Same-store import must terminate at the call-time source frontier even when
//! tiny segments force rotation during pagination — it must never re-import its
//! own freshly-appended output.

use batpak::store::{ImportOptions, ImportSelector, Store, StoreConfig};
use batpak_testkit::prelude::*;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn store_with_tiny_segments(dir: &TempDir) -> TestResult<Store> {
    Ok(Store::open(
        StoreConfig::new(dir.path())
            .with_segment_max_bytes(512)
            .with_sync_every_n_events(1)
            .with_enable_checkpoint(false)
            .with_enable_mmap_index(false),
    )?)
}

#[test]
fn same_store_import_terminates_under_segment_rotation() -> TestResult {
    let dir = TempDir::new()?;
    let store = store_with_tiny_segments(&dir)?;
    let coord = Coordinate::new("entity:import:rotate", "scope:import")?;
    let kind = EventKind::custom(0xF, 0x77);
    let count = 24usize;
    let blob = "x".repeat(280);
    for i in 0..count {
        let _ = store.append(&coord, kind, &serde_json::json!({ "i": i, "blob": blob }))?;
    }
    let before = store.stats().event_count;

    let options = ImportOptions::new("self-rotate")?.with_chunk_size(2);
    let report = store.import_events(&store, &ImportSelector::all(), &options)?;
    assert_eq!(
        report.imported, count as u64,
        "same-store import must import exactly the pre-call user events, then stop"
    );
    assert_eq!(
        store.stats().event_count,
        before + count,
        "import must not amplify beyond one re-application pass"
    );

    let replay = store.import_events(&store, &ImportSelector::all(), &options)?;
    assert_eq!(replay.imported, 0);
    assert_eq!(replay.deduplicated, count as u64);
    assert_eq!(
        store.stats().event_count,
        before + count,
        "second same-store import must be a dedup-only no-op"
    );

    store.close()?;
    Ok(())
}
