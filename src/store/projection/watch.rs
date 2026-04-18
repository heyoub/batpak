use super::flow as projection_flow;
use super::Freshness;
use crate::event::EventSourced;
use crate::store::delivery::subscription::Subscription;
use crate::store::{Open, Store, StoreError};
use std::sync::Arc;

/// Errors that can be reported by [`ProjectionWatcher::recv`].
///
/// Two kinds of observable failure surface here. They are kept separate from
/// the richer [`StoreError`] because a watcher's loop needs to distinguish
/// "the store has gone away" (terminal, stop looping) from "reconstructing
/// the projection reported a transient disk / decode error" (surface to
/// caller, caller decides whether to retry). See G7.
#[derive(Debug)]
#[non_exhaustive]
pub enum WatcherError {
    /// The underlying notification channel closed.
    ///
    /// This can happen because the store dropped, or because the lossy
    /// subscription backing the watcher was pruned after the consumer fell
    /// behind. No further events can ever be delivered on this watcher;
    /// callers should break out of their `recv()` loop.
    StoreClosed,
    /// Re-projecting the entity after a relevant notification failed.
    ///
    /// The underlying error is bubbled up verbatim; this variant is a
    /// classification, not a new error. The watcher itself is still usable —
    /// a caller may choose to retry or terminate.
    Store(StoreError),
}

impl std::fmt::Display for WatcherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StoreClosed => write!(
                f,
                "projection watcher stopped: underlying notification channel closed"
            ),
            Self::Store(e) => write!(f, "projection watcher failed: {e}"),
        }
    }
}

impl std::error::Error for WatcherError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StoreClosed => None,
            Self::Store(e) => Some(e),
        }
    }
}

impl From<StoreError> for WatcherError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

/// Reactive projection watcher: emits updated projections when the entity
/// receives new events. Created via [`Store::watch_projection`].
///
/// Pull-based: the caller drives the loop by calling [`recv()`](Self::recv).
/// Each `recv()` blocks until a new event arrives for the entity, re-projects,
/// and returns the state materialized at the next honest generation.
pub struct ProjectionWatcher<T> {
    sub: Subscription,
    store: Arc<Store<Open>>,
    entity: String,
    freshness: Freshness,
    /// Last generation actually emitted to the caller. Tracked so repeated
    /// notifications that do not advance the generation (e.g. a pure fanout
    /// race where the watcher is woken twice for the same append) do not
    /// re-emit state the caller already consumed. This is generation-based,
    /// not semantic-state-based: an irrelevant append can still advance the
    /// entity generation and therefore produce the same folded state at a
    /// newer watermark. See G7.
    last_delivered_generation: u64,
    /// Startup catch-up flag. If the entity generation advanced while the
    /// watcher subscription was being installed, the first `recv()` must
    /// perform one immediate `project_if_changed` probe before blocking on
    /// the notification channel, otherwise that in-flight append can be
    /// "consumed" by the baseline snapshot and never delivered.
    pending_initial_check: bool,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> ProjectionWatcher<T> {
    pub(crate) fn new(
        sub: Subscription,
        store: Arc<Store<Open>>,
        entity: String,
        freshness: Freshness,
        last_seen_generation: u64,
        pending_initial_check: bool,
    ) -> Self {
        Self {
            sub,
            store,
            entity,
            freshness,
            last_delivered_generation: last_seen_generation,
            pending_initial_check,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> ProjectionWatcher<T>
where
    T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
    T::Input: projection_flow::ReplayInput,
{
    fn wait_for_check_or_notification(&mut self) -> Result<(), WatcherError> {
        if self.pending_initial_check {
            self.pending_initial_check = false;
            return Ok(());
        }
        self.sub.recv().map(|_| ()).ok_or(WatcherError::StoreClosed)
    }

    fn project_next_change(&self) -> Result<Option<(u64, Option<T>)>, WatcherError> {
        projection_flow::project_if_changed::<T, Open>(
            &self.store,
            &self.entity,
            self.last_delivered_generation,
            &self.freshness,
        )
        .map_err(WatcherError::from)
    }

    /// Block until a new event arrives for the watched entity, then re-project
    /// and return the updated state.
    ///
    /// # Return shape
    ///
    /// * `Ok((gen, Some(state)))` — the projection materialized `state` at
    ///   generation `gen`; `gen` is the honest watermark that produced
    ///   `state` (see `ProjectionOutcome::returned_generation`).
    /// * `Ok((gen, None))` — the entity has events at generation `gen` but
    ///   the projection's fold returned `None` (e.g. every relevant event
    ///   cancels out). This is the empty-fold case and is distinct from
    ///   "store closed".
    /// * `Err(WatcherError::StoreClosed)` — the underlying subscription
    ///   channel closed. This is terminal and can happen either because the
    ///   store dropped or because the lossy watcher subscription was pruned
    ///   after the consumer fell behind.
    /// * `Err(WatcherError::Store(e))` — transient reconstruction error
    ///   (e.g. segment read failure). The watcher remains usable.
    ///
    /// # Idempotence across redundant notifications
    ///
    /// A subscription fanout may wake the watcher more than once for the same
    /// committed generation. This method tracks the last delivered
    /// generation and only emits when the new generation is strictly
    /// greater. Redundant notifications for an already-delivered generation
    /// are absorbed silently.
    ///
    /// This deduplicates by generation, not by folded state. If the entity
    /// receives an append that the projection ignores, the watcher still
    /// returns the same state at the newer generation rather than silently
    /// eating the append.
    ///
    /// # Errors
    ///
    /// See the `Return shape` section above for the full failure taxonomy.
    pub fn recv(&mut self) -> Result<(u64, Option<T>), WatcherError> {
        loop {
            // First `recv()` may need to probe immediately if subscription
            // installation raced an append. Every later loop waits for the
            // lossy subscription wakeup channel.
            self.wait_for_check_or_notification()?;

            // `project_if_changed` returns `Ok(None)` when the store's
            // `entity_generation` hasn't moved past `last_delivered_generation`.
            // Any append that advanced generation — including one the
            // projection later ignores — surfaces as `Some((returned_gen,
            // state))`, with the same folded state allowed at a newer honest
            // generation.
            match self.project_next_change()? {
                Some((returned_gen, projected)) => {
                    // Defence-in-depth against re-delivery: even if
                    // `project_if_changed` observed a difference in
                    // `entity_generation`, the honest `returned_gen`
                    // (pulled from the replay plan or cache slot at the
                    // moment the state was materialized) may be equal to
                    // or lower than our last delivery — e.g. a cache hit
                    // for the same generation we already reported. Skip
                    // silently in that case rather than re-emitting.
                    if returned_gen <= self.last_delivered_generation {
                        continue;
                    }
                    self.last_delivered_generation = returned_gen;
                    return Ok((returned_gen, projected));
                }
                None => {
                    // No change since last delivery; loop and wait for the
                    // next subscription event.
                    continue;
                }
            }
        }
    }

    /// Expose the underlying lossy notification receiver for integrations
    /// that need to wait outside [`recv`](Self::recv).
    ///
    /// This is only the wakeup channel. Callers who bypass `recv()` must
    /// reproduce the watcher's own `pending_initial_check` and
    /// `project_if_changed` bookkeeping themselves if they need the same
    /// generation-honest watch semantics.
    #[doc(hidden)]
    pub fn subscription(&self) -> &Subscription {
        &self.sub
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinate::Coordinate;
    use crate::event::{Event, EventKind, JsonValueInput};
    use crate::store::StoreConfig;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    #[derive(Default, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct CountAll(u64);

    impl EventSourced for CountAll {
        type Input = JsonValueInput;

        fn from_events(events: &[Event<serde_json::Value>]) -> Option<Self> {
            (!events.is_empty()).then_some(Self(events.len() as u64))
        }

        fn apply_event(&mut self, _event: &Event<serde_json::Value>) {
            self.0 += 1;
        }

        fn relevant_event_kinds() -> &'static [EventKind] {
            &[]
        }
    }

    #[test]
    fn recv_performs_pending_initial_check_before_blocking_on_subscription() {
        let dir = TempDir::new().expect("temp dir");
        let store = Arc::new(Store::open(StoreConfig::new(dir.path())).expect("open"));
        let coord = Coordinate::new("watch:startup-race", "watch:scope").expect("coord");
        let sub = store.subscribe_lossy(&crate::coordinate::Region::entity("watch:startup-race"));

        store
            .append(
                &coord,
                EventKind::custom(0xF, 1),
                &serde_json::json!({"n": 1}),
            )
            .expect("append");

        let mut watcher = ProjectionWatcher::<CountAll>::new(
            sub,
            Arc::clone(&store),
            "watch:startup-race".to_owned(),
            Freshness::Consistent,
            0,
            true,
        );

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("projection-watch-pending-check-test".to_owned())
            .spawn(move || {
                let result = watcher
                    .recv()
                    .map(|(generation, state)| (generation, state.map(|s| s.0)));
                let _ = tx.send(result);
            })
            .expect("spawn watcher test helper thread");

        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("pending initial check should return without a second append")
            .expect("watcher recv");

        assert!(
            result.0 > 0,
            "generation should advance on the first append"
        );
        assert_eq!(result.1, Some(1));
    }
}
