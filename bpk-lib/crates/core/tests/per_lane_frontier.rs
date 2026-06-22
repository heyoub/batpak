//! Per-lane frontier behavior proofs.
//!
//! PROVES: INV-PER-LANE-FRONTIER. Logical lane frontiers advance independently
//! while the global frontier remains the compatibility max view.
//! CATCHES: lane waits reading the global watermark, cold-start bootstrap
//! dropping non-zero lanes, lane cursor gap observations missing globally hidden
//! ranges, and public frontier views omitting lane entries.
//! SEEDED: explicit lane-1 append, sync, lane waits, and reopen bootstrap.

use batpak::store::delivery::cursor::CursorGapConfig;
use batpak::store::LaneFrontierView;
use batpak_testkit::prelude::*;
use std::num::NonZeroUsize;
use std::time::Duration;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct FrontierCount {
    count: usize,
}

impl EventSourced for FrontierCount {
    type Input = JsonValueInput;

    fn from_events(events: &[ProjectionEvent<Self>]) -> Option<Self> {
        (!events.is_empty()).then_some(Self {
            count: events.len(),
        })
    }

    fn apply_event(&mut self, _event: &ProjectionEvent<Self>) {
        self.count += 1;
    }

    fn relevant_event_kinds() -> &'static [EventKind] {
        &[EventKind::DATA]
    }
}

fn coord() -> TestResult<Coordinate> {
    Ok(Coordinate::new("entity:frontier-lane", "scope:frontier")?)
}

fn append_lane_one(store: &Store) -> TestResult<HlcPoint> {
    store.append_with_options(
        &coord()?,
        EventKind::DATA,
        &serde_json::json!({ "lane": 1 }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let entry = store
        .latest_lane("entity:frontier-lane", 1)
        .ok_or_else(|| std::io::Error::other("expected lane-1 latest entry"))?;
    Ok(HlcPoint {
        wall_ms: entry.wall_ms(),
        global_sequence: entry.global_sequence(),
    })
}

#[test]
fn cancelled_fence_entries_do_not_project_or_bootstrap_as_visible() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let fence = store.begin_visibility_fence()?;
    let mut outbox = fence.outbox();
    outbox.stage_with_options(
        coord()?,
        EventKind::DATA,
        &serde_json::json!({ "lane": 1, "cancelled": true }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let ticket = outbox.submit_flush()?;

    fence.cancel()?;
    let cancelled = ticket.receiver().recv_timeout(Duration::from_secs(2))?;
    assert!(
        matches!(cancelled, Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling a fence must reject the pending fenced batch ticket"
    );
    assert!(
        store.stream_lane("entity:frontier-lane", 1).is_empty(),
        "PROPERTY: cancelled fenced lane-1 entries must not be visible to lane reads"
    );
    let projected: Option<FrontierCount> =
        store.project("entity:frontier-lane", &Freshness::Consistent)?;
    assert_eq!(
        projected, None,
        "PROPERTY: projection replay must not consume cancelled fenced entries"
    );
    let live_lane = store.frontier().lane(1);
    assert_eq!(
        live_lane
            .map(|lane| lane.visible_hlc)
            .unwrap_or(HlcPoint::ORIGIN),
        HlcPoint::ORIGIN,
        "PROPERTY: live lane visible frontier must not advance for cancelled fenced entries"
    );
    assert_eq!(
        live_lane
            .map(|lane| lane.applied_hlc)
            .unwrap_or(HlcPoint::ORIGIN),
        HlcPoint::ORIGIN,
        "PROPERTY: live lane applied frontier must not advance for cancelled fenced entries"
    );

    store.close()?;
    let reopened = Store::open(StoreConfig::new(dir.path()))?;
    let reopened_lane = reopened.frontier().lane(1);
    assert!(
        reopened_lane
            .map(|lane| lane.durable_hlc.global_sequence > 0)
            .unwrap_or(false),
        "PROPERTY: reopen may restore physical durability for cancelled lane entries; visibility/applied remain the reader-safety gates"
    );
    assert_eq!(
        reopened_lane
            .map(|lane| lane.visible_hlc)
            .unwrap_or(HlcPoint::ORIGIN),
        HlcPoint::ORIGIN,
        "PROPERTY: reopen bootstrap must not turn hidden cancelled lane entries into visible progress"
    );
    let projected_after_reopen: Option<FrontierCount> =
        reopened.project("entity:frontier-lane", &Freshness::Consistent)?;
    assert_eq!(
        projected_after_reopen, None,
        "PROPERTY: projection replay after reopen must still skip cancelled fenced entries"
    );
    reopened.close()?;
    Ok(())
}

#[test]
fn lane_cursor_gap_observations_include_global_cancellations() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let coord = coord()?;
    let first = store.append(&coord, EventKind::DATA, &serde_json::json!({ "lane": 0 }))?;

    let fence = store.begin_visibility_fence()?;
    let mut outbox = fence.outbox();
    outbox.stage_with_options(
        coord.clone(),
        EventKind::DATA,
        &serde_json::json!({ "lane": 1, "cancelled": true }),
        AppendOptions::new().with_position_hint(AppendPositionHint::branch_root(1, 0)),
    )?;
    let ticket = outbox.submit_flush()?;
    fence.cancel()?;
    let cancelled = ticket.receiver().recv_timeout(Duration::from_secs(2))?;
    assert!(
        matches!(cancelled, Err(StoreError::VisibilityFenceCancelled)),
        "PROPERTY: cancelling the lane-1 fence rejects the staged write ticket"
    );

    let second = store.append(&coord, EventKind::DATA, &serde_json::json!({ "lane": 0 }))?;
    let mut cursor = store
        .cursor_guaranteed(&Region::entity("entity:frontier-lane").with_lane(0))
        .with_gap_config(CursorGapConfig::Enabled {
            capacity: NonZeroUsize::new(4)
                .ok_or_else(|| std::io::Error::other("expected nonzero gap capacity"))?,
        });

    let delivered = cursor.poll_batch(8);
    let gaps = cursor.take_gaps();

    assert_eq!(
        delivered
            .iter()
            .map(|entry| entry.global_sequence())
            .collect::<Vec<_>>(),
        vec![first.sequence, second.sequence],
        "PROPERTY: lane-0 cursor should deliver the visible lane-0 writes across a lane-1 cancelled range"
    );
    assert_eq!(
        gaps.len(),
        1,
        "PROPERTY: lane-0 cursor must explain the global cancelled range that hides the skipped sequence"
    );
    assert_eq!(
        gaps[0].cancelled_ranges,
        vec![(first.sequence + 1, second.sequence)],
        "PROPERTY: lane cursor gap observations must use global cancelled ranges as well as lane-specific ranges"
    );
    store.close()?;
    Ok(())
}

#[test]
fn lane_frontier_waits_are_scoped_to_lane() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let lane_one_point = append_lane_one(&store)?;
    store.sync()?;

    let frontier = store.frontier();
    let lane_one: LaneFrontierView = frontier
        .lane(1)
        .ok_or_else(|| std::io::Error::other("expected lane-1 frontier view"))?;
    assert!(
        lane_one.accepted_hlc >= lane_one_point,
        "PROPERTY: lane-1 accepted frontier must cover the lane-1 append"
    );
    assert!(
        lane_one.written_hlc >= lane_one_point,
        "PROPERTY: lane-1 written frontier must cover the lane-1 append"
    );
    assert!(
        lane_one.visible_hlc >= lane_one_point,
        "PROPERTY: lane-1 visible frontier must cover the lane-1 append"
    );
    assert!(
        lane_one.durable_hlc >= lane_one_point,
        "PROPERTY: lane-1 durable frontier must advance after sync"
    );
    assert!(
        lane_one.emitted_hlc >= lane_one_point,
        "PROPERTY: lane-1 emitted frontier must cover the lane-1 append"
    );

    store.wait_for_accepted(lane_one_point, Duration::ZERO)?;
    store.wait_for_accepted_lane(1, lane_one_point, Duration::ZERO)?;
    store.wait_for_written(lane_one_point, Duration::ZERO)?;
    store.wait_for_written_lane(1, lane_one_point, Duration::ZERO)?;
    store.wait_for_emitted(lane_one_point, Duration::ZERO)?;
    store.wait_for_emitted_lane(1, lane_one_point, Duration::ZERO)?;
    store.wait_for_visible_lane(1, lane_one_point, Duration::ZERO)?;
    store.wait_for_durable_lane(1, lane_one_point, Duration::ZERO)?;
    let projected: Option<FrontierCount> =
        store.project("entity:frontier-lane", &Freshness::Consistent)?;
    assert_eq!(projected, Some(FrontierCount { count: 1 }));
    store.wait_for_applied_lane(1, lane_one_point, Duration::ZERO)?;
    assert!(
        store
            .wait_for_visible_lane(0, lane_one_point, Duration::ZERO)
            .is_err(),
        "PROPERTY: lane-0 visible wait must not be satisfied by lane-1 progress"
    );

    store.close()?;
    Ok(())
}

#[test]
fn reopen_bootstraps_nonzero_lane_frontier() -> TestResult {
    let dir = TempDir::new()?;
    let store = Store::open(StoreConfig::new(dir.path()))?;
    let lane_one_point = append_lane_one(&store)?;
    store.sync()?;
    store.close()?;

    let reopened = Store::open(StoreConfig::new(dir.path()))?;
    let lane_one = reopened
        .frontier()
        .lane(1)
        .ok_or_else(|| std::io::Error::other("expected reopened lane-1 frontier view"))?;

    assert!(
        lane_one.visible_hlc >= lane_one_point,
        "PROPERTY: reopen must restore lane-1 visible frontier from persisted dag_lane"
    );
    assert!(
        lane_one.durable_hlc >= lane_one_point,
        "PROPERTY: reopen must restore lane-1 durable frontier from persisted dag_lane"
    );
    reopened.wait_for_visible_lane(1, lane_one_point, Duration::ZERO)?;

    reopened.close()?;
    Ok(())
}
