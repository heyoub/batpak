use super::*;

impl<State> Store<State> {
    /// Return the current operator-facing frontier view.
    pub fn frontier(&self) -> FrontierView {
        self.watermark_handle.lock().snapshot_view()
    }

    /// Return a coherent clone of the internal frontier watermarks.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_watermark_snapshot(&self) -> FrontierView {
        self.watermark_handle.lock().snapshot_view()
    }

    /// Register a projection ID in the applied-frontier registry.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_register_projection(&self, projection_id: &str) {
        self.projection_registry.register(projection_id.to_owned());
    }

    /// Register the same projection ID used by `project::<T>()` for `entity`.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_register_projection_for<T: 'static>(&self, entity: &str) {
        self.projection_registry
            .register(ProjectionRegistry::id_for_type::<T>(entity));
    }

    /// Report projection progress directly for focused frontier tests.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_notify_projection_applied(&self, projection_id: &str, point: HlcPoint) {
        self.projection_registry
            .notify_applied(projection_id.to_owned(), point);
    }

    /// Remove a projection ID from the applied-frontier registry.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_unregister_projection(&self, projection_id: &str) {
        self.projection_registry.unregister(projection_id);
    }

    /// Wake frontier waiters without advancing a watermark.
    #[cfg(any(test, feature = "dangerous-test-hooks"))]
    pub fn dangerous_notify_watermark_waiters(&self) {
        self.watermark_handle.dangerous_notify_all();
    }
}
