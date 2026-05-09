use crate::store::stats::HlcPoint;
use crate::store::write::writer::WatermarkAdvanceHandle;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Default)]
struct ProjectionRegistryState {
    progress: HashMap<String, HlcPoint>,
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

    pub(crate) fn notify_applied(&self, projection_id: impl Into<String>, point: HlcPoint) {
        let projection_id = projection_id.into();
        let mut state = self.inner.lock();
        let progress = state.progress.entry(projection_id).or_insert(point);
        *progress = (*progress).max(point);
        self.recompute_locked(&state);
    }

    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub(crate) fn unregister(&self, projection_id: &str) {
        let mut state = self.inner.lock();
        state.progress.remove(projection_id);
        self.recompute_locked(&state);
    }

    fn recompute_locked(&self, state: &ProjectionRegistryState) {
        if let Some(point) = state.progress.values().copied().min() {
            self.watermark_handle.lock().set_applied(point);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectionRegistry;

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
}
