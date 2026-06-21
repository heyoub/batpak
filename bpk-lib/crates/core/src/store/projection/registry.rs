use crate::store::stats::HlcPoint;
use crate::store::write::writer::WatermarkAdvanceHandle;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Default)]
struct ProjectionRegistryState {
    progress: HashMap<String, HlcPoint>,
    lane_progress: HashMap<(String, u32), HlcPoint>,
}

/// Tracks per-projection progress and advances the global applied frontier to
/// the slowest registered projection.
#[derive(Clone)]
pub(crate) struct ProjectionRegistry {
    inner: Arc<Mutex<ProjectionRegistryState>>,
    watermark_handle: WatermarkAdvanceHandle,
}

impl ProjectionRegistry {
    pub(crate) fn new(watermark_handle: WatermarkAdvanceHandle) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ProjectionRegistryState::default())),
            watermark_handle,
        }
    }

    pub(crate) fn id_for_type<T: 'static>(entity: &str) -> String {
        format!("{}:{entity}", std::any::type_name::<T>())
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn register(&self, projection_id: impl Into<String>) {
        let current_applied = self.watermark_handle.lock().snapshot().applied_hlc;
        let mut state = self.inner.lock();
        state
            .progress
            .entry(projection_id.into())
            .or_insert(current_applied);
        self.recompute_locked(&state);
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn notify_applied(&self, projection_id: impl Into<String>, point: HlcPoint) {
        let projection_id = projection_id.into();
        let mut state = self.inner.lock();
        let progress = state.progress.entry(projection_id).or_insert(point);
        *progress = (*progress).max_by_sequence(point);
        self.recompute_locked(&state);
    }

    pub(crate) fn notify_applied_on_lane(
        &self,
        projection_id: impl Into<String>,
        lane: u32,
        point: HlcPoint,
    ) {
        let projection_id = projection_id.into();
        let frontier = self.watermark_handle.lock().snapshot_view();
        let current_applied = frontier.applied_hlc;
        let current_lane_applied = frontier
            .lane(lane)
            .map(|lane| lane.applied_hlc)
            .unwrap_or(HlcPoint::ORIGIN);
        let mut state = self.inner.lock();
        let progress = state
            .progress
            .entry(projection_id.clone())
            .or_insert(current_applied);
        *progress = (*progress).max_by_sequence(point);
        let lane_progress = state
            .lane_progress
            .entry((projection_id, lane))
            .or_insert(current_lane_applied);
        *lane_progress = (*lane_progress).max_by_sequence(point);
        self.recompute_locked(&state);
        self.recompute_lane_locked(&state, lane);
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn unregister(&self, projection_id: &str) {
        let mut state = self.inner.lock();
        state.progress.remove(projection_id);
        state
            .lane_progress
            .retain(|(registered_projection_id, _), _| registered_projection_id != projection_id);
        self.recompute_locked(&state);
    }

    fn recompute_locked(&self, state: &ProjectionRegistryState) {
        if let Some(point) = state
            .progress
            .values()
            .copied()
            .reduce(HlcPoint::min_by_sequence)
        {
            self.watermark_handle.lock().set_applied(point);
        }
    }

    fn recompute_lane_locked(&self, state: &ProjectionRegistryState, lane: u32) {
        if let Some(point) = state
            .lane_progress
            .iter()
            .filter_map(|((_, progress_lane), point)| (*progress_lane == lane).then_some(*point))
            .reduce(HlcPoint::min_by_sequence)
        {
            self.watermark_handle
                .lock()
                .set_applied_on_lane(lane, point);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectionRegistry;
    use crate::store::stats::HlcPoint;
    use crate::store::write::writer::WatermarkState;
    use crate::store::SystemClock;
    use std::sync::Arc;

    struct ProjectionA;
    struct ProjectionB;

    #[test]
    fn projection_ids_include_type_and_entity_identity() {
        let a = ProjectionRegistry::id_for_type::<ProjectionA>("entity:projection-id");
        let b = ProjectionRegistry::id_for_type::<ProjectionB>("entity:projection-id");

        assert!(a.contains("ProjectionA"));
        assert!(a.contains("entity:projection-id"));
        assert_ne!(
            a, b,
            "PROPERTY: different projection types on the same entity must not share an applied-frontier ID"
        );
    }

    #[test]
    fn unregister_removes_only_the_target_projections_lane_progress() {
        // Kills registry.rs:87 `!=` -> `==`: `unregister` must retain the lane
        // progress of OTHER projections and drop only the target's. Flipping the
        // predicate keeps ONLY the target, so the surviving projection's frontier
        // is lost and a subsequent lane recompute regresses to the dropped entry.
        let origin = HlcPoint::ORIGIN;
        let low = HlcPoint {
            wall_ms: 100,
            global_sequence: 10,
        };
        let mid = HlcPoint {
            wall_ms: 200,
            global_sequence: 20,
        };
        let high = HlcPoint {
            wall_ms: 300,
            global_sequence: 30,
        };
        let handle = WatermarkState::bootstrap_handle(high, Arc::new(SystemClock::new()));
        handle
            .lock()
            .reset_to_bootstrap_lanes(high, high, [(1, high)], [(1, high)]);
        // Lower the lane applied frontier below visible so newly-registered
        // projections inherit a low baseline (visible stays HIGH so no frontier
        // invariant is violated when applied later regresses).
        handle.lock().set_applied_on_lane(1, origin);
        let registry = ProjectionRegistry::new(handle.clone());

        // "drop" registers first at LOW, then "keep" registers at HIGH; the lane
        // applied frontier is the min over both = LOW.
        registry.notify_applied_on_lane("projection:drop", 1, low);
        registry.notify_applied_on_lane("projection:keep", 1, high);

        registry.unregister("projection:drop");

        // Force a lane recompute via the surviving "keep" projection. With the
        // correct retain, only "keep"=HIGH remains, so the lane rises to HIGH.
        // Under the `==` mutant, "keep" was dropped and "drop"=LOW survives, so
        // the recompute regresses the lane back down to LOW.
        registry.notify_applied_on_lane("projection:keep", 1, mid);

        let lane = handle
            .lock()
            .snapshot_view()
            .lane(1)
            .expect("lane 1 frontier exists");
        assert_eq!(
            lane.applied_hlc, high,
            "PROPERTY: unregister must drop only the target projection and preserve the survivor's lane frontier"
        );
    }

    #[test]
    fn first_lane_projection_notification_does_not_regress_existing_lane_applied_frontier() {
        let high = HlcPoint {
            wall_ms: 100,
            global_sequence: 10,
        };
        let low = HlcPoint {
            wall_ms: 1,
            global_sequence: 4,
        };
        let handle = WatermarkState::bootstrap_handle(high, Arc::new(SystemClock::new()));
        handle
            .lock()
            .reset_to_bootstrap_lanes(high, high, [(1, high)], [(1, high)]);
        let registry = ProjectionRegistry::new(handle.clone());

        registry.notify_applied_on_lane("projection:first-seen", 1, low);

        let lane = handle
            .lock()
            .snapshot_view()
            .lane(1)
            .expect("lane 1 frontier exists");
        assert_eq!(
            lane.applied_hlc, high,
            "PROPERTY: first notification from an unregistered projection must not regress a lane's applied frontier"
        );
    }
}
