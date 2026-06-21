use super::{
    HlcPoint, LaneWatermarks, SystemClock, WatermarkAdvanceHandle, WatermarkKind, WatermarkState,
};
use crate::store::StoreError;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn clock() -> Arc<SystemClock> {
    Arc::new(SystemClock::new())
}

fn handle() -> WatermarkAdvanceHandle {
    WatermarkState::handle(clock())
}

fn point(global_sequence: u64) -> HlcPoint {
    HlcPoint {
        wall_ms: global_sequence,
        global_sequence,
    }
}

/// Advance a single global watermark `kind` to `point`, leaving the others
/// at their current value so wrappers can be pinned to their own kind.
fn advance_global(state: &mut WatermarkState, kind: WatermarkKind, point: HlcPoint) {
    match kind {
        WatermarkKind::Accepted => state.advance_accepted_on_lane(0, point),
        WatermarkKind::Written => state.advance_written_on_lane(0, point),
        WatermarkKind::Durable => {
            state.advance_accepted_on_lane(0, point);
            state.advance_written_on_lane(0, point);
            state.advance_durable(point);
        }
        WatermarkKind::Visible => state.advance_visible_on_lane(0, point),
        WatermarkKind::Applied => state.set_applied(point),
        WatermarkKind::Emitted => state.advance_emitted_on_lane(0, point),
    }
}

#[test]
fn for_bootstrap_seeds_lane_watermarks_from_point_not_origin() {
    // LaneWatermarks::for_bootstrap must seed every per-lane watermark from
    // `point`. The `for_bootstrap -> Default::default()` mutant would leave
    // them all at ORIGIN.
    let seeded = point(42);
    let lane = LaneWatermarks::for_bootstrap(seeded);

    assert_eq!(lane.current(WatermarkKind::Accepted), seeded);
    assert_eq!(lane.current(WatermarkKind::Written), seeded);
    assert_eq!(lane.current(WatermarkKind::Durable), seeded);
    assert_eq!(lane.current(WatermarkKind::Visible), seeded);
    assert_eq!(lane.current(WatermarkKind::Applied), seeded);
    assert_eq!(
        lane.current(WatermarkKind::Emitted),
        seeded,
        "PROPERTY: bootstrap lane watermarks seed from point, not ORIGIN"
    );
}

#[test]
fn state_for_bootstrap_seeds_globals_and_lane_zero_from_point() {
    // WatermarkState::for_bootstrap must seed the global watermarks AND
    // insert lane 0 seeded from `point`. The `for_bootstrap ->
    // Default::default()` mutant would leave globals at ORIGIN and the lane
    // map empty.
    let seeded = point(99);
    let state = WatermarkState::for_bootstrap(seeded, clock());
    let snapshot = state.snapshot();

    assert_eq!(snapshot.accepted_hlc, seeded);
    assert_eq!(snapshot.written_hlc, seeded);
    assert_eq!(snapshot.durable_hlc, seeded);
    assert_eq!(snapshot.visible_hlc, seeded);
    assert_eq!(snapshot.applied_hlc, seeded);
    assert_eq!(snapshot.emitted_hlc, seeded);
    assert_eq!(
        state.lane_watermark(0).current(WatermarkKind::Accepted),
        seeded,
        "PROPERTY: bootstrap state seeds lane 0 from point, not ORIGIN/empty"
    );
}

#[test]
fn lane_view_reports_signed_visible_minus_durable() {
    // visible_minus_durable_seq = visible - durable. Choosing visible <
    // durable yields a NEGATIVE value; the `- -> +` mutant would report a
    // positive sum instead.
    let mut state = WatermarkState::new(clock());
    // durable at 10, visible at 3 -> 3 - 10 = -7 (mutant would be +13).
    // Keep all frontier invariants valid: accepted >= written >= durable,
    // accepted >= visible, emitted >= visible, visible >= applied.
    state.advance_accepted_on_lane(1, point(10));
    state.advance_written_on_lane(1, point(10));
    state.advance_durable(point(10));
    state.advance_visible_on_lane(1, point(3));
    state.advance_emitted_on_lane(1, point(3));

    let view = state
        .snapshot_view()
        .lanes
        .into_iter()
        .find(|view| view.lane == 1)
        .expect("lane 1 present");

    assert_eq!(
        view.visible_minus_durable_seq, -7,
        "PROPERTY: visible_minus_durable is the signed difference visible - durable"
    );
}

#[test]
fn advance_visible_and_emitted_moves_both_watermarks() {
    // advance_visible_and_emitted must move BOTH the visible and emitted
    // global watermarks; the no-op mutant would leave them at ORIGIN.
    let mut state = WatermarkState::new(clock());
    let target = point(17);
    state.advance_accepted_on_lane(0, target);
    state.advance_visible_and_emitted(target);
    let snapshot = state.snapshot();

    assert_eq!(
        snapshot.visible_hlc, target,
        "PROPERTY: advance_visible_and_emitted advances the visible watermark"
    );
    assert_eq!(
        snapshot.emitted_hlc, target,
        "PROPERTY: advance_visible_and_emitted advances the emitted watermark"
    );
}

#[test]
fn wait_for_accepted_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    // Advance a DIFFERENT kind (written) so accepted is still at ORIGIN.
    advance_global(&mut handle.lock(), WatermarkKind::Written, point(5));

    let result = handle.wait_for_accepted(point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Accepted,
                ..
            })
        ),
        "PROPERTY: wait_for_accepted must block on the Accepted watermark, not return Ok"
    );
}

#[test]
fn wait_for_written_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    advance_global(&mut handle.lock(), WatermarkKind::Accepted, point(5));

    let result = handle.wait_for_written(point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Written,
                ..
            })
        ),
        "PROPERTY: wait_for_written must block on the Written watermark, not return Ok"
    );
}

#[test]
fn wait_for_emitted_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    advance_global(&mut handle.lock(), WatermarkKind::Visible, point(5));

    let result = handle.wait_for_emitted(point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Emitted,
                ..
            })
        ),
        "PROPERTY: wait_for_emitted must block on the Emitted watermark, not return Ok"
    );
}

#[test]
fn wait_for_accepted_on_lane_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    handle.lock().advance_written_on_lane(2, point(5));

    let result = handle.wait_for_accepted_on_lane(2, point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Accepted,
                ..
            })
        ),
        "PROPERTY: wait_for_accepted_on_lane must block on the lane Accepted watermark"
    );
}

#[test]
fn wait_for_written_on_lane_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    handle.lock().advance_accepted_on_lane(2, point(5));

    let result = handle.wait_for_written_on_lane(2, point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Written,
                ..
            })
        ),
        "PROPERTY: wait_for_written_on_lane must block on the lane Written watermark"
    );
}

#[test]
fn wait_for_durable_on_lane_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    handle.lock().advance_accepted_on_lane(2, point(5));

    let result = handle.wait_for_durable_on_lane(2, point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Durable,
                ..
            })
        ),
        "PROPERTY: wait_for_durable_on_lane must block on the lane Durable watermark"
    );
}

#[test]
fn wait_for_applied_on_lane_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    handle.lock().advance_visible_on_lane(2, point(5));

    let result = handle.wait_for_applied_on_lane(2, point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Applied,
                ..
            })
        ),
        "PROPERTY: wait_for_applied_on_lane must block on the lane Applied watermark"
    );
}

#[test]
fn wait_for_emitted_on_lane_times_out_when_only_other_kinds_advance() {
    let handle = handle();
    handle.lock().advance_visible_on_lane(2, point(5));

    let result = handle.wait_for_emitted_on_lane(2, point(5), Duration::from_millis(1));
    assert!(
        matches!(
            result,
            Err(StoreError::WaitTimeout {
                watermark: WatermarkKind::Emitted,
                ..
            })
        ),
        "PROPERTY: wait_for_emitted_on_lane must block on the lane Emitted watermark"
    );
}

#[test]
fn wait_for_watermark_on_lane_satisfied_at_equal_point_returns_ok() {
    // current == point must be SATISFIED (covers_sequence is >=). This pins
    // the satisfied path returning Ok at the equality boundary.
    let handle = handle();
    handle.lock().advance_accepted_on_lane(4, point(8));

    let result = handle.wait_for_accepted_on_lane(4, point(8), Duration::from_millis(1));
    assert!(
        result.is_ok(),
        "PROPERTY: a lane watermark exactly at the target point is satisfied"
    );
}

#[test]
fn wait_for_watermark_on_lane_blocks_for_full_timeout_before_timing_out() {
    // The `elapsed >= timeout -> elapsed < timeout` mutant would return the
    // timeout error IMMEDIATELY instead of waiting. Assert the real wait
    // blocks for (most of) the timeout before returning the error.
    let handle = handle();
    // Use the Visible lane wrapper (not in the writer-commit mutant set) so
    // this case isolates the `elapsed >= timeout` branch from the per-kind
    // wrapper mutants. Leave lane 4 Visible at ORIGIN; target is unreachable.
    let timeout = Duration::from_millis(150);
    let started = Instant::now();
    let result = handle.wait_for_visible_on_lane(4, point(1), timeout);
    let elapsed = started.elapsed();

    assert!(
        matches!(result, Err(StoreError::WaitTimeout { .. })),
        "PROPERTY: an unreachable lane wait must time out"
    );
    assert!(
            elapsed >= Duration::from_millis(100),
            "PROPERTY: the wait must block until the timeout elapses, not return instantly (elapsed = {elapsed:?})"
        );
}

#[test]
fn lane_durable_uses_sequence_axis_not_hlc_wall_order() {
    let mut state = WatermarkState::new(Arc::new(SystemClock::new()));
    let written_high_sequence_low_wall = HlcPoint {
        wall_ms: 1,
        global_sequence: 10,
    };
    let durable_low_sequence_high_wall = HlcPoint {
        wall_ms: 9_999,
        global_sequence: 5,
    };

    state.advance_accepted_on_lane(1, written_high_sequence_low_wall);
    state.advance_written_on_lane(1, written_high_sequence_low_wall);
    state.advance_durable(durable_low_sequence_high_wall);

    assert_eq!(
            state.lane_watermark(1).durable_hlc,
            HlcPoint::ORIGIN,
            "PROPERTY: physical durability must not cover a lane write whose global_sequence is above the synced sequence, even when its wall_ms sorts lower"
        );
}

#[test]
fn lane_visible_uses_sequence_axis_not_hlc_wall_order() {
    let mut state = WatermarkState::new(Arc::new(SystemClock::new()));
    let low_sequence_high_wall = HlcPoint {
        wall_ms: 9_999,
        global_sequence: 5,
    };
    let high_sequence_low_wall = HlcPoint {
        wall_ms: 1,
        global_sequence: 10,
    };

    state.advance_accepted_on_lane(1, high_sequence_low_wall);
    state.advance_visible_on_lane(1, low_sequence_high_wall);
    state.advance_visible_on_lane(1, high_sequence_low_wall);

    assert_eq!(
        state.lane_watermark(1).visible_hlc,
        high_sequence_low_wall,
        "PROPERTY: lane visibility must advance by global_sequence, not HLC wall order"
    );
}

#[test]
fn bootstrap_lane_durable_merge_uses_sequence_axis() {
    let mut state = WatermarkState::new(Arc::new(SystemClock::new()));
    let low_sequence_high_wall = HlcPoint {
        wall_ms: 9_999,
        global_sequence: 5,
    };
    let high_sequence_low_wall = HlcPoint {
        wall_ms: 1,
        global_sequence: 10,
    };

    state.reset_to_bootstrap_lanes(
        high_sequence_low_wall,
        high_sequence_low_wall,
        [(1, low_sequence_high_wall)],
        [(1, high_sequence_low_wall)],
    );

    assert_eq!(
            state.lane_watermark(1).durable_hlc,
            high_sequence_low_wall,
            "PROPERTY: bootstrap lane durable must cover lane visible by global_sequence, not HLC wall order"
        );
}
