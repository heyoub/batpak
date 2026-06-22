#![cfg(feature = "dangerous-test-hooks")]
//! PROVES: INV-FRONTIER-OPEN-MONOTONIC and INV-FRONTIER-MONOTONIC for the store
//! open/close/reopen lifecycle. Immediately after mutable `Store::open`, the
//! lifecycle open event seeds accepted, written, durable, visible, and emitted to
//! the same HLC point. Explicit `close` emits exactly one
//! `SYSTEM_CLOSE_COMPLETED`; `Drop` emits none; reopen consumes the recorded
//! close frontier. Restart bootstrap is monotonic across mutable and read-only
//! reopen, even under configured clock skew.
//!
//! CATCHES: missing handle plumbing, missing public accessor coverage, a
//! bootstrap snapshot that does not reflect `SYSTEM_OPEN_COMPLETED`, or a
//! non-monotonic open/close frontier accepted on reopen. The adversarial forged
//! close-frame regression lives in `durable_frontier_semantics_close_regression`.
//!
//! SEEDED: deterministic tempdir-based open (with fixed-clock skew variants).

use batpak_testkit::durable_frontier_semantics as dfs_support;

use batpak::prelude::{EventKind, Region};
use batpak::store::{FrontierView, HlcPoint, ReadOnly, Store, StoreConfig};
use dfs_support::*;
use tempfile::TempDir;

fn fixed_clock_config(dir: &TempDir, now_us: i64) -> StoreConfig {
    StoreConfig::new(dir.path()).with_clock_fn(move || now_us)
}

fn lifecycle_open_count<State: batpak::store::StoreState>(store: &Store<State>) -> usize {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.event_kind() == EventKind::SYSTEM_OPEN_COMPLETED)
        .count()
}

fn lifecycle_close_entries<State: batpak::store::StoreState>(
    store: &Store<State>,
) -> Vec<batpak::store::index::IndexEntry> {
    store
        .query(&Region::entity("batpak:store"))
        .into_iter()
        .filter(|entry| entry.event_kind() == EventKind::SYSTEM_CLOSE_COMPLETED)
        .collect()
}

#[test]
fn bootstrap_watermark_snapshot_matches_lifecycle_open_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");

    let snapshot: FrontierView = store.dangerous_watermark_snapshot();
    let frontier: FrontierView = store.diagnostics().frontier;
    let open_hlc = snapshot.durable_hlc;

    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);
    assert_eq!(snapshot.accepted_hlc, open_hlc);
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);

    assert_eq!(frontier.durable_hlc, open_hlc);
    assert_eq!(frontier.visible_hlc, open_hlc);
    assert_eq!(frontier.visible_minus_durable_seq, 0);
    assert_eq!(frontier.oldest_pending_write_age_ms, None);
}

#[test]
fn open_after_close_advances_open_hlc_past_max_pre_close() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-reopen");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-reopen"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    let snapshot = reopened.dangerous_watermark_snapshot();
    let open_hlc = snapshot.accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: mutable reopen lifecycle HLC must advance past pre-close max; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
    assert_eq!(snapshot.written_hlc, open_hlc);
    assert_eq!(snapshot.durable_hlc, open_hlc);
    assert_eq!(snapshot.visible_hlc, open_hlc);
    assert_eq!(snapshot.emitted_hlc, open_hlc);
    assert_eq!(snapshot.applied_hlc, open_hlc);
}

#[test]
fn read_only_reopen_does_not_emit_lifecycle_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly-lifecycle");

    let (max_hlc_before_read_only, lifecycle_count_before_read_only) = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly-lifecycle"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        let lifecycle_count = lifecycle_open_count(&store);
        assert_eq!(lifecycle_count, 1);
        store.close().expect("close");
        (max_hlc, lifecycle_count)
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert_eq!(
        lifecycle_open_count(&read_only),
        lifecycle_count_before_read_only,
        "PROPERTY: read-only open must not append SYSTEM_OPEN_COMPLETED"
    );
    assert!(snapshot.accepted_hlc >= max_hlc_before_read_only);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}

#[test]
fn explicit_close_emits_system_close_completed_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-event");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-close-event"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let close_entries = lifecycle_close_entries(&read_only);

    assert_eq!(
        close_entries.len(),
        1,
        "PROPERTY: explicit close must emit exactly one SYSTEM_CLOSE_COMPLETED event"
    );
    assert!(
        point(&close_entries[0]) >= max_hlc_before_close,
        "PROPERTY: close lifecycle HLC must cover all visible events at close; close={:?}, max={max_hlc_before_close:?}",
        point(&close_entries[0])
    );
}

#[test]
fn drop_without_explicit_close_emits_no_close_event() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-drop-no-close");

    {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
    }

    {
        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        assert!(
            lifecycle_close_entries(&read_only).is_empty(),
            "PROPERTY: Drop must not emit SYSTEM_CLOSE_COMPLETED"
        );
    }

    let reopened = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
    assert!(
        reopened.frontier().accepted_hlc > HlcPoint::ORIGIN,
        "PROPERTY: reopen without a close event must still bootstrap from recovered events and wall-time floor"
    );
}

#[test]
fn bootstrap_open_hlc_consumes_recorded_close_hlc() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-close-bootstrap");

    let close_hlc_1 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 1);
        point(&close_entries[0])
    };

    let close_hlc_2 = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("reopen store");
        assert!(
            store.frontier().accepted_hlc >= close_hlc_1,
            "PROPERTY: reopen must consume the recorded close frontier"
        );
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 2}))
            .expect("append");
        store.close().expect("close");

        let read_only = Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path()))
            .expect("open read-only");
        let close_entries = lifecycle_close_entries(&read_only);
        assert_eq!(close_entries.len(), 2);
        let first = point(&close_entries[0]);
        let second = point(&close_entries[1]);
        assert!(
            second >= first,
            "PROPERTY: repeated graceful closes must advance monotonically; first={first:?}, second={second:?}"
        );
        second
    };

    let third = Store::open(StoreConfig::new(dir.path())).expect("third open");
    let open_hlc = third.frontier().accepted_hlc;
    assert!(open_hlc >= close_hlc_1);
    assert!(open_hlc >= close_hlc_2);
}

#[test]
fn bootstrap_with_clock_skew_preserves_monotonicity() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-clock-skew");

    let max_hlc_before_close = {
        let store = Store::open(fixed_clock_config(&dir, 9_000_000_000)).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-clock-skew"));
        assert_eq!(entries.len(), 1);
        let max_hlc = point(&entries[0]);
        store.close().expect("close");
        max_hlc
    };

    let reopened = Store::open(fixed_clock_config(&dir, 1_000_000)).expect("reopen store");
    let open_hlc = reopened.dangerous_watermark_snapshot().accepted_hlc;

    assert!(
        open_hlc > max_hlc_before_close,
        "PROPERTY: reopen must remain monotonic even when the configured clock moves backward; open={open_hlc:?}, max={max_hlc_before_close:?}"
    );
}

#[test]
fn empty_store_open_starts_with_lifecycle_frontier_then_append_advances() {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
    let open_hlc = store.dangerous_watermark_snapshot().accepted_hlc;
    assert!(open_hlc > HlcPoint::ORIGIN);
    assert_eq!(open_hlc.global_sequence, 0);

    let coord = coord("entity:frontier-empty-advance");
    let _ = store
        .append(&coord, kind(), &serde_json::json!({"n": 1}))
        .expect("append");
    let snapshot = store.dangerous_watermark_snapshot();
    assert!(snapshot.accepted_hlc > open_hlc);
}

#[test]
fn read_only_open_bootstraps_frontier_from_rebuilt_index() {
    let dir = TempDir::new().expect("temp dir");
    let coord = coord("entity:frontier-readonly");

    let max_hlc_before_close = {
        let store = Store::open(StoreConfig::new(dir.path())).expect("open store");
        let _ = store
            .append(&coord, kind(), &serde_json::json!({"n": 1}))
            .expect("append");
        let entries = store.query(&Region::entity("entity:frontier-readonly"));
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        let point = HlcPoint {
            wall_ms: entry.wall_ms(),
            global_sequence: entry.global_sequence(),
        };
        assert!(point > HlcPoint::ORIGIN);
        store.close().expect("close");
        point
    };

    let read_only =
        Store::<ReadOnly>::open_read_only(StoreConfig::new(dir.path())).expect("open read-only");
    let snapshot = read_only.dangerous_watermark_snapshot();

    assert!(snapshot.accepted_hlc >= max_hlc_before_close);
    assert_eq!(snapshot.written_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.durable_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.visible_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.emitted_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.applied_hlc, snapshot.accepted_hlc);
    assert_eq!(snapshot.oldest_pending_write_age_ms, None);
}
