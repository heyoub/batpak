// justifies: INV-TEST-PANIC-AS-ASSERTION; historical fixture tests copy immutable store directories to temp space and use panic/assert as the invariant signal.
#![allow(clippy::panic)]

//! PROVES:
//!   - ADR-0009 historical cold-start artifact fixtures remain readable by
//!     current `Store::open`.
//!   - Older checkpoint/mmap artifacts that do not directly carry
//!     receipt-extension maps hydrate those maps from authoritative `.fbat`
//!     frames.
//!
//! CATCHES: accidental removal of older artifact readers, restore path drift,
//! missing receipt-extension hydration for historical snapshots, or fixture
//! sprawl that no longer opens through the real store lifecycle.
//!
//! SEEDED: checked-in fixture stores under `tests/fixtures/cold_start/adr0009`.

use batpak::prelude::*;
use batpak::store::cold_start::rebuild::OpenIndexPath;
use batpak::store::ExtensionKey;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/cold_start/adr0009"
);

fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create fixture copy destination");
    for entry in fs::read_dir(from).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let source = entry.path();
        let destination = to.join(entry.file_name());
        let file_type = entry.file_type().expect("read fixture file type");
        if file_type.is_dir() {
            copy_dir_all(&source, &destination);
        } else if file_type.is_file() {
            fs::copy(&source, &destination).expect("copy fixture file");
        }
    }
}

fn assert_historical_fixture_opens(
    fixture_name: &str,
    expected_path: OpenIndexPath,
    enable_mmap_index: bool,
    lane: u32,
    depth: u32,
) {
    let tmp = TempDir::new().expect("temp dir");
    copy_dir_all(&Path::new(FIXTURE_ROOT).join(fixture_name), tmp.path());

    let store = Store::open(
        StoreConfig::new(tmp.path())
            .with_enable_checkpoint(true)
            .with_enable_mmap_index(enable_mmap_index),
    )
    .expect("open historical fixture");
    let report = store
        .diagnostics()
        .open_report
        .expect("historical fixture open should produce diagnostics");
    assert_eq!(report.path, expected_path);

    let coord = Coordinate::new(format!("entity:adr0009-{fixture_name}"), "scope:fixture")
        .expect("fixture coordinate");
    let entries = store.by_entity(coord.entity());
    assert_eq!(
        entries.len(),
        1,
        "fixture should expose exactly one app event for {}",
        coord.entity()
    );
    assert_eq!(entries[0].dag_lane(), lane);
    assert_eq!(entries[0].dag_depth(), depth);

    let stored = store
        .get(batpak::id::EventId::from(entries[0].event_id()))
        .expect("fetch fixture event");
    assert_eq!(stored.event.header.position.lane(), lane);
    assert_eq!(stored.event.header.position.depth(), depth);

    let extension_key = ExtensionKey::new("app.audit").expect("extension key");
    let replay = store
        .append_with_options(
            &coord,
            EventKind::DATA,
            &serde_json::json!({"fixture": "replay"}),
            AppendOptions::new()
                .with_idempotency(batpak::id::IdempotencyKey::from(0xAD00 + u128::from(lane)))
                .with_position_hint(AppendPositionHint::new(99, 99))
                .with_extension(extension_key.clone(), vec![0xFF]),
        )
        .expect("idempotent replay from historical fixture");
    assert_eq!(
        replay.extensions.get(&extension_key),
        Some(&vec![
            u8::try_from(lane).expect("fixture lane fits u8"),
            u8::try_from(depth).expect("fixture depth fits u8")
        ]),
        "older artifact restore must hydrate receipt extensions from the backing .fbat frame"
    );
    assert!(store.verify_append_receipt(&replay));
}

#[test]
fn historical_checkpoint_v5_fixture_opens_and_hydrates_extensions() {
    assert_historical_fixture_opens("checkpoint-v5", OpenIndexPath::Checkpoint, false, 5, 3);
}

#[test]
fn historical_mmap_v4_fixture_opens_and_hydrates_extensions() {
    assert_historical_fixture_opens("mmap-v4", OpenIndexPath::Mmap, true, 7, 2);
}
