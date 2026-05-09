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
    let actual_lane = stored.event.header.position.lane();
    let actual_depth = stored.event.header.position.depth();
    assert_eq!(actual_lane, lane);
    assert_eq!(actual_depth, depth);
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
    assert!(stored.event.header.position.wall_ms() > 0);
    assert_eq!(stored.event.header.position.sequence(), 0);
}

#[test]
fn append_position_hint_default_is_root() {
    let hint = AppendPositionHint::default();
    assert_eq!(hint.lane, 0);
    assert_eq!(hint.depth, 0);
}

#[test]
fn dag_position_public_surface_reports_expected_values() {
    let module_root = batpak::coordinate::position::DagPosition::root();
    let root = DagPosition::root();
    let child = DagPosition::child_at(5, 1_234, 7);
    let forked = DagPosition::fork(2, 9);

    let module_root_is_root = module_root.is_root();
    let root_is_root = root.is_root();
    let child_wall_ms = child.wall_ms();
    let child_counter = child.counter();
    let child_depth = child.depth();
    let child_lane = child.lane();
    let child_sequence = child.sequence();
    let root_is_ancestor = root.is_ancestor_of(&child);
    let child_is_ancestor = child.is_ancestor_of(&forked);

    assert!(
        module_root_is_root,
        "PROPERTY: the public coordinate::position module export must expose DagPosition::root()"
    );
    assert!(
        root_is_root,
        "PROPERTY: DagPosition::root() must report is_root()"
    );
    assert_eq!(child_wall_ms, 1_234);
    assert_eq!(child_counter, 7);
    assert_eq!(child_depth, 0);
    assert_eq!(child_lane, 0);
    assert_eq!(child_sequence, 5);
    assert!(
        root_is_ancestor,
        "PROPERTY: root position must be an ancestor of a same-lane child"
    );
    assert!(
        !child_is_ancestor,
        "PROPERTY: positions on different depth/lane branches must not report ancestorhood"
    );
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
fn batch_append_preserves_per_item_position_hints() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = test_coord();

    let receipts = store
        .append_batch(vec![
            BatchAppendItem::new(
                coord.clone(),
                data_kind(),
                &json!({"batch": 0}),
                AppendOptions::new().with_position_hint(AppendPositionHint::new(2, 1)),
                CausationRef::None,
            )
            .expect("batch item 0"),
            BatchAppendItem::new(
                coord.clone(),
                data_kind(),
                &json!({"batch": 1}),
                AppendOptions::new().with_position_hint(AppendPositionHint::new(5, 3)),
                CausationRef::None,
            )
            .expect("batch item 1"),
        ])
        .expect("append batch with position hints");

    let first = store
        .get(receipts[0].event_id)
        .expect("fetch first batch item");
    let second = store
        .get(receipts[1].event_id)
        .expect("fetch second batch item");

    assert_position(&first, 2, 1);
    assert_position(&second, 5, 3);
}

#[test]
fn idempotent_replay_preserves_original_position_hint() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let coord = test_coord();
    let key = 0xABCD_EF01_2345_6789_u128;

    let first = store
        .append_with_options(
            &coord,
            data_kind(),
            &json!({"x": 1}),
            AppendOptions::new()
                .with_idempotency(key)
                .with_position_hint(AppendPositionHint::new(4, 2)),
        )
        .expect("first append");
    let replay = store
        .append_with_options(
            &coord,
            data_kind(),
            &json!({"x": 2}),
            AppendOptions::new()
                .with_idempotency(key)
                .with_position_hint(AppendPositionHint::new(9, 9)),
        )
        .expect("idempotent replay");

    assert_eq!(replay.event_id, first.event_id);
    assert_eq!(replay.sequence, first.sequence);

    let stored = store
        .get(first.event_id)
        .expect("fetch idempotent original event");
    assert_position(&stored, 4, 2);
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
        reopened
            .diagnostics()
            .open_report
            .as_ref()
            .expect("mmap reopen should report its open path")
            .path,
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
        reopened
            .diagnostics()
            .open_report
            .as_ref()
            .expect("checkpoint reopen should report its open path")
            .path,
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
        reopened
            .diagnostics()
            .open_report
            .as_ref()
            .expect("rebuild reopen should report its open path")
            .path,
        OpenIndexPath::Rebuild
    );
}
