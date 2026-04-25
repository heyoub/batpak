// justifies: INV-TEST-PANIC-AS-ASSERTION; this frontier bootstrap harness uses panic! through assert macros for crisp invariant failures.
#![allow(clippy::panic)]
#![cfg(feature = "dangerous-test-hooks")]

//! PROVES:
//!   - Step-1 frontier scaffolding compiles and exposes a coherent dangerous snapshot.
//!   - Immediately after mutable `Store::open`, the lifecycle open event seeds
//!     accepted, written, durable, visible, and emitted to the same HLC point.
//!
//! CATCHES: missing handle plumbing, missing public accessor coverage, or a
//! bootstrap snapshot that does not reflect `SYSTEM_OPEN_COMPLETED`.
//!
//! SEEDED: deterministic tempdir-based open.

use batpak::prelude::{Coordinate, EventKind, Region};
use batpak::store::{FrontierView, HlcPoint, ReadOnly, Store, StoreConfig, WatermarkSnapshot};
use tempfile::TempDir;

#[test]
fn bootstrap_watermark_snapshot_matches_lifecycle_open_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let snapshot: WatermarkSnapshot = store.dangerous_watermark_snapshot();
    let frontier: FrontierView = store.diagnostics().frontier;
    let open_hlc = snapshot.durable_hlc;

    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(snapshot.accepted_hlc, open_hlc);
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, HlcPoint::ORIGIN);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);

    assert_eq!(frontier.durable_hlc, open_hlc);
    assert_eq!(frontier.current_visible_hlc, open_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
}

#[test]
fn single_append_cadence_gt_1_visible_exceeds_durable_frontier() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let bootstrap = store.dangerous_watermark_snapshot();
    let coord = Coordinate::new("entity:frontier", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x90);

    let receipt = store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append");

    let visible = store.query(&Region::entity("entity:frontier"));
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].event_id, receipt.event_id);

    let snapshot = store.dangerous_watermark_snapshot();
    let frontier = store.diagnostics().frontier;

    assert!(snapshot.visible_hlc > snapshot.durable_hlc);
    assert!(snapshot.accepted_hlc >= snapshot.written_hlc);
    assert!(snapshot.written_hlc >= snapshot.visible_hlc);
    assert_eq!(snapshot.durable_hlc, bootstrap.durable_hlc);
    assert_eq!(snapshot.applied_hlc, HlcPoint::ORIGIN);
    assert_eq!(snapshot.emitted_hlc, snapshot.visible_hlc);
    assert!(snapshot.oldest_pending_write_age_ms.is_some());

    assert_eq!(frontier.current_visible_hlc, snapshot.visible_hlc);
    assert_eq!(frontier.durable_hlc, snapshot.durable_hlc);
    assert!(frontier.visible_minus_durable_seq > 0);
    assert!(frontier.oldest_pending_write_age_ms.is_some());
}

#[test]
fn explicit_sync_advances_durable_and_clears_pending_write_age() {
    let dir = TempDir::new().expect("temp dir");
    let config = StoreConfig::new(dir.path()).with_sync_every_n_events(1000);
    let store = Store::open(config).expect("open store");
    let coord = Coordinate::new("entity:frontier-sync", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x91);

    store
        .append(&coord, kind, &serde_json::json!({"n": 1}))
        .expect("append");

    let before_sync = store.dangerous_watermark_snapshot();
    assert!(before_sync.visible_hlc > before_sync.durable_hlc);
    assert!(before_sync.oldest_pending_write_age_ms.is_some());

    store.sync().expect("sync");

    let after_sync = store.dangerous_watermark_snapshot();
    assert_eq!(after_sync.durable_hlc, after_sync.accepted_hlc);
    assert_eq!(after_sync.durable_hlc, after_sync.visible_hlc);
    assert_eq!(after_sync.oldest_pending_write_age_ms, None);
    assert_eq!(
        store.diagnostics().frontier.oldest_pending_write_age_ms,
        None
    );
}

#[test]
fn read_only_open_bootstraps_frontier_from_rebuilt_index() {
    let dir = TempDir::new().expect("temp dir");
    let coord = Coordinate::new("entity:frontier-readonly", "scope:test").expect("coord");
    let kind = EventKind::custom(0xF, 0x92);

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        store
            .append(&coord, kind, &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly"));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let point = HlcPoint {
            wall_ms: entry.wall_ms,
            global_sequence: entry.global_sequence,
        };
        assert!(point > HlcPoint::ORIGIN);
        store.close().expect("close");
        point
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert_eq!(snapshot.accepted_hlc, max_hlc_before_close);
    assert_eq!(snapshot.written_hlc, max_hlc_before_close);
    assert_eq!(snapshot.durable_hlc, max_hlc_before_close);
    assert_eq!(snapshot.visible_hlc, max_hlc_before_close);
    assert_eq!(snapshot.emitted_hlc, max_hlc_before_close);
    assert_eq!(snapshot.applied_hlc, HlcPoint::ORIGIN);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}
