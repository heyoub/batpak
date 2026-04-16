use batpak::prelude::*;
use batpak::store::OpenIndexPath;
use serde_json::json;
use tempfile::TempDir;

fn test_coord() -> Coordinate {
    Coordinate::new("entity:dag-position", "scope:test").expect("valid coordinate")
}

fn data_kind() -> EventKind {
    EventKind::DATA
}

fn assert_position(stored: &StoredEvent<serde_json::Value>, lane: u32, depth: u32) {
    assert_eq!(stored.event.header.position.lane, lane);
    assert_eq!(stored.event.header.position.depth, depth);
}

#[test]
fn explicit_lane_depth_survives_into_committed_header() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = test_coord();
    let hint = AppendPositionHint::new(3, 1);
    let opts = AppendOptions::new().with_position_hint(hint);

    let receipt = store
        .append_with_options(&coord, data_kind(), &json!({"x": 1}), opts)
        .expect("append with hint");
    let stored = store.get(receipt.event_id).expect("fetch stored event");

    assert_position(&stored, hint.lane, hint.depth);
    assert!(stored.event.header.position.wall_ms > 0);
    assert_eq!(stored.event.header.position.sequence, 0);
}

#[test]
fn append_position_hint_default_is_root() {
    let hint = AppendPositionHint::default();
    assert_eq!(hint.lane, 0);
    assert_eq!(hint.depth, 0);
}

#[test]
fn no_position_hint_preserves_root_position() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = test_coord();

    let receipt = store
        .append(&coord, data_kind(), &json!({}))
        .expect("append default");
    let stored = store.get(receipt.event_id).expect("fetch stored event");

    assert_position(&stored, 0, 0);
}

#[test]
fn lane_depth_survives_store_reopen_via_mmap() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(true);
    let event_id = {
        let store = Store::open(config.clone()).expect("open store");
        let receipt = store
            .append_with_options(
                &test_coord(),
                data_kind(),
                &json!({"path": "mmap"}),
                AppendOptions::new().with_position_hint(AppendPositionHint::new(7, 2)),
            )
            .expect("append with hint");
        store.close().expect("close store");
        receipt.event_id
    };

    let reopened = Store::open(config).expect("reopen store");
    let stored = reopened.get(event_id).expect("fetch reopened event");
    assert_position(&stored, 7, 2);
    assert_eq!(
        reopened.diagnostics().open_report.as_ref().unwrap().path,
        OpenIndexPath::Mmap
    );
}

#[test]
fn lane_depth_survives_store_reopen_via_checkpoint() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(true)
        .with_enable_mmap_index(false);
    let event_id = {
        let store = Store::open(config.clone()).expect("open store");
        let receipt = store
            .append_with_options(
                &test_coord(),
                data_kind(),
                &json!({"path": "checkpoint"}),
                AppendOptions::new().with_position_hint(AppendPositionHint::new(5, 4)),
            )
            .expect("append with hint");
        store.close().expect("close store");
        receipt.event_id
    };

    let reopened = Store::open(config).expect("reopen store");
    let stored = reopened.get(event_id).expect("fetch reopened event");
    assert_position(&stored, 5, 4);
    assert_eq!(
        reopened.diagnostics().open_report.as_ref().unwrap().path,
        OpenIndexPath::Checkpoint
    );
}

#[test]
fn lane_depth_survives_full_rebuild_without_snapshots() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path())
        .with_enable_checkpoint(false)
        .with_enable_mmap_index(false);
    let event_id = {
        let store = Store::open(config.clone()).expect("open store");
        let receipt = store
            .append_with_options(
                &test_coord(),
                data_kind(),
                &json!({"path": "rebuild"}),
                AppendOptions::new().with_position_hint(AppendPositionHint::new(9, 6)),
            )
            .expect("append with hint");
        store.close().expect("close store");
        receipt.event_id
    };

    let reopened = Store::open(config).expect("reopen store");
    let stored = reopened.get(event_id).expect("fetch reopened event");
    assert_position(&stored, 9, 6);
    assert_eq!(
        reopened.diagnostics().open_report.as_ref().unwrap().path,
        OpenIndexPath::Rebuild
    );
}
