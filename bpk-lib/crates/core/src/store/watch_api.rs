use super::*;
use std::time::Duration;

fn generation_advanced_after_subscribe(baseline: u64, post_subscribe: u64) -> bool {
    post_subscribe > baseline
}

impl Store<Open> {
    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe_lossy(&self, region: &Region) -> Subscription {
        // justifies: INV-TYPESTATE-OPEN-HAS-WRITER; Store<Open> typestate guarantees writer presence at
        // construction (see Store::open_with_cache in src/store/lifecycle.rs — it fails the open
        // instead of yielding Store<Open> if the writer cannot be spawned).
        // The expect here documents an invariant, it does not recover from
        // one: observing None means the store is mid-drop and every public
        // path through Store<Open> is already invalid.
        let rx = self
            .writer_ref()
            .subscribers
            .subscribe_with_region(self.config.broadcast_capacity, region.clone());
        Subscription::new(rx, region.clone())
    }

    /// REACT: spawn a background thread running the subscribe→react→append loop.
    /// Returns a JoinHandle. The thread runs until the store is dropped (subscription closes).
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the background thread cannot be spawned.
    pub fn react_loop<R>(
        self: &Arc<Self>,
        region: &Region,
        reactor: R,
    ) -> Result<std::thread::JoinHandle<()>, StoreError>
    where
        R: crate::event::sourcing::Reactive<serde_json::Value> + Send + 'static,
    {
        let store = Arc::clone(self);
        let region = region.clone();
        let sub = self
            .writer_ref()
            .reactor_subscribers
            .subscribe_with_region(self.config.broadcast_capacity, region.clone());
        std::thread::Builder::new()
            .name("batpak-reactor".into())
            .spawn(move || {
                while let Ok(envelope) = sub.recv() {
                    let notif = envelope.notification;
                    for (coord, kind, payload) in reactor.react(&envelope.stored.event) {
                        if let Err(e) = store.append_reaction(
                            &coord,
                            kind,
                            &payload,
                            notif.correlation_id,
                            notif.event_id,
                        ) {
                            tracing::warn!("react_loop: failed to append reaction: {e}");
                        }
                    }
                }
            })
            .map_err(StoreError::Io)
    }

    /// WATCH: reactive projection subscription. Returns a `ProjectionWatcher`
    /// that re-projects `T` when new events arrive for `entity`.
    ///
    /// Internally subscribes to entity events, then re-projects on each notification.
    /// The watcher is pull-based: the caller drives the loop via
    /// [`ProjectionWatcher::recv`], which returns
    /// `Result<(u64, Option<T>), WatcherError>` — see the method docs for the
    /// full three-way return taxonomy (materialized state, empty fold, store
    /// closed, or watcher closure after the lossy/prunable subscription is
    /// dropped). The returned generation is persisted honestly: redundant
    /// wakeups for an already-delivered generation are suppressed, but an
    /// append that advances the entity generation can still yield the same
    /// folded state if `T::relevant_event_kinds()` filters it out.
    ///
    /// Requires `Arc<Store>` because the watcher outlives the borrow.
    pub fn watch_projection<T>(
        self: &Arc<Self>,
        entity: &str,
        freshness: Freshness,
    ) -> ProjectionWatcher<T>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    {
        let baseline_generation = self.entity_generation(entity).unwrap_or(0);
        let sub = self.subscribe_lossy(&Region::entity(entity));
        let post_subscribe_generation = self.entity_generation(entity).unwrap_or(0);
        let store = Arc::clone(self);
        let entity_owned = entity.to_owned();
        ProjectionWatcher::new(
            sub,
            store,
            entity_owned,
            freshness,
            baseline_generation,
            generation_advanced_after_subscribe(baseline_generation, post_subscribe_generation),
        )
    }

    /// WATCH: cursor-backed reactive projection subscription.
    ///
    /// This returns a guaranteed-delivery watcher over an ordered cursor
    /// instead of a lossy subscription. A cursor-backed watcher cannot be
    /// pruned under fanout backpressure; its `recv()` method therefore
    /// returns `CursorWatcherError`, which has no subscription-pruned branch.
    ///
    /// When `checkpoint_id` is provided, the cursor resumes from its durable
    /// checkpoint. Without a checkpoint it starts from the beginning of the
    /// current in-memory index, so the first `recv()` can materialize already
    /// committed entity state instead of relying on a startup catch-up probe.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the checkpoint-bound cursor cannot be
    /// constructed, for example because an existing checkpoint belongs to a
    /// different region or is corrupt.
    pub fn watch_projection_with_cursor<T>(
        self: &Arc<Self>,
        entity: &str,
        freshness: Freshness,
        checkpoint_id: Option<CheckpointId>,
    ) -> Result<ProjectionWatcher<T, Cursor>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    {
        let region = Region::entity(entity);
        let cursor = match checkpoint_id {
            Some(id) => Cursor::new_with_checkpoint(
                region,
                Arc::clone(&self.index),
                &self.config.data_dir,
                &id,
            )?,
            None => self.cursor_guaranteed(&region),
        };
        Ok(ProjectionWatcher::new_cursor(
            cursor,
            Arc::clone(self),
            entity.to_owned(),
            freshness,
            0,
            Duration::from_millis(10),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_advanced_after_subscribe_is_strictly_forward() {
        assert!(
            !generation_advanced_after_subscribe(7, 7),
            "PROPERTY: equal baseline/post-subscribe generations must not trigger an initial watcher catch-up"
        );
        assert!(
            generation_advanced_after_subscribe(7, 8),
            "PROPERTY: a post-subscribe generation above baseline must trigger the initial watcher catch-up"
        );
        assert!(
            !generation_advanced_after_subscribe(8, 7),
            "PROPERTY: older post-subscribe observations must never trigger an initial watcher catch-up"
        );
    }
}
