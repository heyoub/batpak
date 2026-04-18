use super::flow as projection_flow;
use super::Freshness;
use crate::event::EventSourced;
use crate::store::delivery::subscription::Subscription;
use crate::store::{Open, Store, StoreError};
use std::sync::Arc;

/// Reactive projection watcher: emits updated projections when the entity
/// receives new events. Created via [`Store::watch_projection`].
///
/// Pull-based: the caller drives the loop by calling [`recv()`](Self::recv).
/// Each `recv()` blocks until a new event arrives for the entity, re-projects,
/// and returns the updated state. Returns `None` when the store is dropped.
pub struct ProjectionWatcher<T> {
    sub: Subscription,
    store: Arc<Store<Open>>,
    entity: String,
    freshness: Freshness,
    last_seen_generation: u64,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> ProjectionWatcher<T> {
    pub(crate) fn new(
        sub: Subscription,
        store: Arc<Store<Open>>,
        entity: String,
        freshness: Freshness,
        last_seen_generation: u64,
    ) -> Self {
        Self {
            sub,
            store,
            entity,
            freshness,
            last_seen_generation,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> ProjectionWatcher<T>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: projection_flow::ReplayInput,
{
    /// Block until a new event arrives for the watched entity, then re-project
    /// and return the updated state. Returns `None` if the store is dropped
    /// (subscription channel closed) or if projection returns no state.
    ///
    /// # Errors
    /// Returns `StoreError` if the projection fails (e.g., segment read error).
    pub fn recv(&mut self) -> Result<Option<T>, StoreError> {
        loop {
            if self.sub.recv().is_none() {
                return Ok(None);
            }

            if let Some((generation, projected)) = projection_flow::project_if_changed::<T, Open>(
                &self.store,
                &self.entity,
                self.last_seen_generation,
                &self.freshness,
            )? {
                self.last_seen_generation = generation;
                return Ok(projected);
            }

            let (generation, projected) = projection_flow::project_with_generation::<T, Open>(
                &self.store,
                &self.entity,
                &self.freshness,
            )?;
            self.last_seen_generation = generation;
            if projected.is_some() || generation > 0 {
                return Ok(projected);
            }
        }
    }

    /// Expose the underlying subscription's receiver for async integration.
    /// After receiving a notification, call `project()` on the store manually.
    #[doc(hidden)]
    pub fn subscription(&self) -> &Subscription {
        &self.sub
    }
}
