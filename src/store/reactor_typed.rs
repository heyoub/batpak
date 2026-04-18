//! Typed reactor surface and shared internal canal runner (Dispatch Chapter
//! T4b, with seams for T6).
//!
//! This module houses three layers:
//!
//!   1. **Public surface.** [`ReactorConfig`], [`ReactorError`],
//!      [`TypedReactorHandle`], and the `Store::react_loop_typed<T, R>`
//!      method. End users build `R: TypedReactive<T>` reactors and hand
//!      them to `Store::react_loop_typed`.
//!
//!   2. **Shared internal runner.** [`ReactorDispatcher`] (crate-private
//!      trait) + [`run_reactor`] (crate-private function). The runner rides
//!      the canal chosen by ADR-0011 — `cursor_guaranteed` via
//!      `cursor_worker` — and funnels every reactor shape through a single
//!      implementation. T4b's single-kind reactor and T6's multi-kind
//!      reactor are both expressed as specific `ReactorDispatcher` impls.
//!
//!   3. **T4b's adapter.** [`SingleKindDispatcher<T, R>`] wraps a
//!      `TypedReactive<T>` reactor and implements [`ReactorDispatcher`].
//!      T6 will add a parallel `MultiKindDispatcher<R>` adapter alongside.
//!
//! **Decode-failure contract (unified across T4b and T6).**
//!
//!   * Wrong kind (`route_typed` returns `Ok(None)`) → dispatcher returns
//!     `Ok(())` with an empty batch; runner advances the checkpoint without
//!     invoking user code. Normal filter, not an error.
//!   * Matched kind + decode failure (`route_typed` returns
//!     `Err(TypedDecodeError)`) → dispatcher returns
//!     `Err(ReactorStepError::Decode(_))`. Runner surfaces
//!     `ReactorError::Decode` through the join handle. **Hard correctness
//!     signal, never a silent skip.**
//!   * User handler returns `Err(E)` → dispatcher returns
//!     `Err(ReactorStepError::User(_))`. Runner surfaces
//!     `ReactorError::User` through the join handle.
//!
//! Error semantics are surfaced via a shared error slot
//! (`Arc<Mutex<Option<ReactorError<E>>>>`). On step error the runner
//! stashes the error and returns [`CursorWorkerAction::StopWithRollback`]
//! — a first-class rollback action on the cursor-worker enum that rolls
//! the cursor back to the last committed checkpoint and stops the
//! worker cleanly. No `panic!`-as-control-flow; the error surfaces via
//! [`TypedReactorHandle::join`]'s typed return.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::coordinate::Region;
use crate::event::sourcing::TypedReactive;
use crate::event::{DecodeTyped, EventPayload, StoredEvent, TypedDecodeError};
use crate::store::delivery::cursor::{CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle};
use crate::store::reaction::ReactionBatch;
use crate::store::{Open, RestartPolicy, Store, StoreError};

/// Configuration for [`Store::react_loop_typed`] and
/// [`Store::react_loop_multi`].
///
/// Mirrors the underlying [`CursorWorkerConfig`] fields that apply to
/// typed reactor loops. Fields that do not belong on a reactor surface
/// (e.g. the cursor worker's internal budget callback) are intentionally
/// excluded.
///
/// # Delivery semantics
///
/// The reactor loop rides the pull-based cursor canal (ADR-0011), so
/// matched-kind events are delivered at-least-once within process
/// lifetime. There is no exactly-once delivery: the canal does not
/// coordinate delivery with the user handler's side effects, and
/// handler-side idempotency is the caller's responsibility. A future
/// durable-checkpoint option (see `CursorWorkerConfig::checkpoint_id`)
/// extends at-least-once across restarts; the reactor surface does not
/// expose that option today.
#[derive(Clone, Debug)]
pub struct ReactorConfig {
    /// Max events per cursor poll. Each event is dispatched individually
    /// and its `ReactionBatch` is flushed atomically before the next
    /// event is processed; batching here only affects cursor-poll size,
    /// not the reaction-flush granularity.
    pub batch_size: usize,
    /// Sleep when no matching events are available.
    pub idle_sleep: Duration,
    /// Restart policy for panics that escape the user's handler.
    ///
    /// Scope:
    ///
    /// * Governs ONLY panics caught by the cursor-worker's
    ///   `catch_unwind` — i.e. the handler panicked rather than
    ///   returning a typed error.
    /// * Explicit `Err` returns from the handler (`ReactorError::User`,
    ///   `ReactorError::Decode`) stop the loop immediately regardless
    ///   of restart policy — they are correctness signals, not
    ///   transient failures. Retrying a handler that deterministically
    ///   returned `Err` would just loop forever.
    /// * Matched-kind decode failure is always immediate-stop. Decode
    ///   mismatch means the persisted event's kind matched `T::KIND`
    ///   but the payload could not be deserialised — a schema or
    ///   encoding bug that restart cannot fix.
    /// * Store-level errors observed during event fetch or
    ///   reaction-batch flush are also immediate-stop: they surface as
    ///   `ReactorError::Store` through the join handle.
    ///
    /// On each caught panic the worker rolls the cursor back to the
    /// last committed checkpoint (so the failing batch is re-delivered)
    /// and consumes one restart slot. If the budget is exhausted the
    /// runner surfaces `ReactorError::RestartBudgetExhausted` via the
    /// join handle.
    pub restart_policy: RestartPolicy,
}

impl Default for ReactorConfig {
    fn default() -> Self {
        Self {
            batch_size: 64,
            idle_sleep: Duration::from_millis(10),
            restart_policy: RestartPolicy::Once,
        }
    }
}

/// Error returned by [`TypedReactorHandle::join`].
#[derive(Debug)]
pub enum ReactorError<E: std::error::Error + Send + Sync + 'static> {
    /// The reactor handler returned `Err(E)` for an event of matched kind.
    User(E),
    /// Store error during event fetch or reaction-batch flush.
    Store(StoreError),
    /// Matched-kind decode failure: the event's kind matched but the payload
    /// could not be deserialized. Always a correctness signal — never a
    /// silent skip. Unified across T4b and T6.
    Decode(TypedDecodeError),
    /// The cursor-worker restart policy budget was exhausted — the
    /// worker panicked more often than `restart_policy` allowed, and
    /// the runner stopped the loop rather than restart indefinitely.
    /// Treat as a signal that the handler is deterministically failing
    /// in a way that bounded restarts cannot paper over.
    RestartBudgetExhausted,
}

impl<E: std::error::Error + Send + Sync + 'static> std::fmt::Display for ReactorError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User(e) => write!(f, "reactor user error: {e}"),
            Self::Store(e) => write!(f, "reactor store error: {e}"),
            Self::Decode(e) => write!(f, "reactor decode failure (matched kind): {e}"),
            Self::RestartBudgetExhausted => write!(f, "reactor restart budget exhausted"),
        }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> std::error::Error for ReactorError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::User(e) => Some(e),
            Self::Store(e) => Some(e),
            Self::Decode(e) => Some(e),
            Self::RestartBudgetExhausted => None,
        }
    }
}

/// Handle for a running typed reactor loop.
pub struct TypedReactorHandle<E: std::error::Error + Send + Sync + 'static> {
    inner: CursorWorkerHandle,
    error_slot: Arc<Mutex<Option<ReactorError<E>>>>,
}

impl<E: std::error::Error + Send + Sync + 'static> TypedReactorHandle<E> {
    /// Request a clean stop. The loop exits after the current dispatch.
    pub fn stop(&self) {
        self.inner.stop();
    }

    /// Wait passively for the reactor loop to stop on its own.
    ///
    /// `join` does NOT signal stop. It blocks until the worker exits
    /// because the handler returned an error, the restart budget was
    /// exhausted, or a sibling called `stop()`. Use
    /// [`stop_and_join`](Self::stop_and_join) when you want to signal
    /// stop and wait in a single call.
    ///
    /// # Errors
    /// Returns [`ReactorError::User`], [`ReactorError::Decode`],
    /// [`ReactorError::Store`], or [`ReactorError::RestartBudgetExhausted`]
    /// if the loop terminated due to a step failure or exhausted its
    /// restart budget. Returns `Ok(())` on a clean stop.
    pub fn join(self) -> Result<(), ReactorError<E>> {
        let Self { inner, error_slot } = self;
        // Drop cursor-worker `WriterCrashed` result — we already captured
        // richer errors in the slot. If the thread panicked without
        // populating the slot, surface it as a Store/WriterCrashed error.
        let worker_result = inner.join();
        let stashed = error_slot.lock().take();
        match (stashed, worker_result) {
            (Some(err), _) => Err(err),
            (None, Ok(())) => Ok(()),
            (None, Err(e)) => Err(ReactorError::Store(e)),
        }
    }

    /// Signal stop, then wait for the reactor loop to exit.
    ///
    /// Semantically equivalent to calling `stop()` immediately followed
    /// by `join()`, but expressed as one call so callers do not need to
    /// worry about the handle being consumed by `join` before `stop`
    /// could be called.
    ///
    /// # Errors
    /// Same as [`join`](Self::join).
    pub fn stop_and_join(self) -> Result<(), ReactorError<E>> {
        let Self { inner, error_slot } = self;
        let worker_result = inner.stop_and_join();
        let stashed = error_slot.lock().take();
        match (stashed, worker_result) {
            (Some(err), _) => Err(err),
            (None, Ok(())) => Ok(()),
            (None, Err(e)) => Err(ReactorError::Store(e)),
        }
    }
}

// ─── Shared internal runner ──────────────────────────────────────────────────

/// A dispatch step outcome from a [`ReactorDispatcher`].
pub(crate) enum ReactorStepError<E> {
    /// Matched-kind handler returned an error.
    User(E),
    /// Matched-kind decode failed.
    Decode(TypedDecodeError),
}

/// Internal dispatcher trait: all typed reactor shapes (T4b's
/// [`SingleKindDispatcher`], T6's `MultiKindDispatcher<_>`) implement
/// this. Users never see it — they implement [`TypedReactive<T>`] (T4b)
/// or [`MultiReactive<Input>`] (T6). Parameterised over the replay-lane
/// payload type so both JSON (`serde_json::Value`) and raw msgpack
/// (`Vec<u8>`) lanes share one runner.
pub(crate) trait ReactorDispatcher<P>: Send + 'static {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Inspect `event`, optionally push reactions into `out`.
    ///
    /// Return `Ok(())` for both matched-kind-success and
    /// wrong-kind-filtered events — they are indistinguishable to the
    /// runner. The runner advances the checkpoint and flushes `out` when
    /// `Ok(())` is returned with a non-empty batch.
    fn dispatch(
        &mut self,
        event: &StoredEvent<P>,
        out: &mut ReactionBatch,
    ) -> Result<(), ReactorStepError<Self::Error>>;
}

/// Shared canal runner: spawns a `cursor_worker` and drives it with the
/// given dispatcher. Parameterised over the replay-lane payload type so
/// both `react_loop_typed` / `react_loop_multi` (JSON) and
/// `react_loop_multi_raw` (raw msgpack) flow through the same code.
///
/// `fetch` is the lane-specific "get event by ID" function —
/// [`Store::get`] for JSON, [`Store::get_raw`] for msgpack.
///
/// On step error the runner stashes the error in the shared slot and
/// returns [`CursorWorkerAction::StopWithRollback`] so the cursor rolls
/// back to the last committed checkpoint and stops. No panics — the
/// worker exits cleanly and [`TypedReactorHandle::join`] reads the
/// stashed typed error.
pub(crate) fn run_reactor<P, D>(
    store: &Arc<Store<Open>>,
    region: &Region,
    config: ReactorConfig,
    mut dispatcher: D,
    fetch: fn(&Store<Open>, u128) -> Result<StoredEvent<P>, StoreError>,
) -> Result<TypedReactorHandle<D::Error>, StoreError>
where
    P: Send + 'static,
    D: ReactorDispatcher<P>,
{
    let error_slot: Arc<Mutex<Option<ReactorError<D::Error>>>> = Arc::new(Mutex::new(None));
    let slot_for_handler = Arc::clone(&error_slot);
    let slot_for_budget = Arc::clone(&error_slot);
    let store_for_handler = Arc::clone(store);

    // D1: supply the cursor-worker with a callback that writes
    // `ReactorError::RestartBudgetExhausted` into the shared slot
    // before the worker exits. The callback is `FnOnce`; the worker
    // fires it at most once on the budget-exhaustion path.
    let on_budget_exhausted: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
        let mut guard = slot_for_budget.lock();
        if guard.is_none() {
            *guard = Some(ReactorError::RestartBudgetExhausted);
        }
    });

    let worker_config = CursorWorkerConfig {
        batch_size: config.batch_size,
        idle_sleep: config.idle_sleep,
        restart: config.restart_policy,
        checkpoint_id: None,
        on_restart_budget_exhausted: Some(on_budget_exhausted),
    };

    let inner = store.cursor_worker(region, worker_config, move |entries, inner_store| {
        for entry in entries {
            // Fetch the full event using the lane's specific reader.
            let stored = match fetch(inner_store, entry.event_id) {
                Ok(s) => s,
                Err(e) => {
                    *slot_for_handler.lock() = Some(ReactorError::Store(e));
                    return CursorWorkerAction::StopWithRollback;
                }
            };

            let mut batch = ReactionBatch::new();
            let step = dispatcher.dispatch(&stored, &mut batch);

            match step {
                Ok(()) => {
                    if !batch.is_empty() {
                        if let Err(e) = batch.flush(
                            &store_for_handler,
                            stored.event.header.correlation_id,
                            stored.event.header.event_id,
                        ) {
                            *slot_for_handler.lock() = Some(ReactorError::Store(e));
                            return CursorWorkerAction::StopWithRollback;
                        }
                    }
                }
                Err(ReactorStepError::User(e)) => {
                    *slot_for_handler.lock() = Some(ReactorError::User(e));
                    return CursorWorkerAction::StopWithRollback;
                }
                Err(ReactorStepError::Decode(e)) => {
                    *slot_for_handler.lock() = Some(ReactorError::Decode(e));
                    return CursorWorkerAction::StopWithRollback;
                }
            }
        }
        CursorWorkerAction::Continue
    })?;

    Ok(TypedReactorHandle { inner, error_slot })
}

// ─── T4b single-kind adapter ──────────────────────────────────────────────────

/// Internal adapter that turns a `TypedReactive<T>` reactor into a
/// `ReactorDispatcher`. Wrong-kind events return `Ok(())` with no batch
/// output; matched-kind events invoke the user handler.
pub(crate) struct SingleKindDispatcher<T: EventPayload, R: TypedReactive<T>> {
    reactor: R,
    _marker: std::marker::PhantomData<fn(T) -> T>,
}

impl<T: EventPayload, R: TypedReactive<T>> SingleKindDispatcher<T, R> {
    pub(crate) fn new(reactor: R) -> Self {
        Self {
            reactor,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T, R> ReactorDispatcher<serde_json::Value> for SingleKindDispatcher<T, R>
where
    T: EventPayload + Send + 'static,
    R: TypedReactive<T>,
{
    type Error = R::Error;

    fn dispatch(
        &mut self,
        event: &StoredEvent<serde_json::Value>,
        out: &mut ReactionBatch,
    ) -> Result<(), ReactorStepError<Self::Error>> {
        // Decode-failure contract:
        //   Ok(None)  → wrong kind → silent filter (no user call, no error)
        //   Ok(Some)  → matched kind + decode ok → call handler
        //   Err(_)    → matched kind + decode fail → correctness signal
        let routed = event.event.route_typed::<T>();
        match routed {
            Ok(None) => Ok(()),
            Ok(Some(t)) => {
                // Hand a typed StoredEvent<T> to the handler. The header
                // (event_id, correlation, causation, position) comes from
                // the source event unchanged; only the payload is swapped
                // for the decoded T.
                let typed_stored = StoredEvent {
                    coordinate: event.coordinate.clone(),
                    event: crate::event::Event {
                        header: event.event.header.clone(),
                        payload: t,
                        hash_chain: event.event.hash_chain.clone(),
                    },
                };
                self.reactor
                    .react(&typed_stored, out)
                    .map_err(ReactorStepError::User)
            }
            Err(e) => Err(ReactorStepError::Decode(e)),
        }
    }
}

// ─── Store::react_loop_typed (public) ─────────────────────────────────────────

impl Store<Open> {
    /// Spawn a typed reactor loop over the pull-based cursor canal
    /// (per ADR-0011).
    ///
    /// The reactor handler is called once per event whose kind matches
    /// `T::KIND`; wrong-kind events in the region are filtered silently.
    /// On `Ok(())` from the handler, any reactions staged in the
    /// [`ReactionBatch`] are flushed in a single batch append with the
    /// source event's correlation and causation IDs (atomic w.r.t. the
    /// append: either every staged item lands or none does). On `Err(E)`
    /// the batch is dropped (no partial commits) and the loop stops —
    /// surfacing [`ReactorError::User`] through the returned handle.
    /// Matched-kind decode failures stop the loop with
    /// [`ReactorError::Decode`]; wrong-kind filtering is never treated
    /// as an error.
    ///
    /// Delivery is at-least-once within process lifetime — a crash
    /// between handler success and the next successful batch means the
    /// current batch is re-delivered on restart. The handler must be
    /// idempotent relative to the side effects it performs.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the background cursor-worker thread
    /// cannot be spawned.
    pub fn react_loop_typed<T, R>(
        self: &Arc<Self>,
        region: &Region,
        config: ReactorConfig,
        reactor: R,
    ) -> Result<TypedReactorHandle<R::Error>, StoreError>
    where
        T: EventPayload + Send + 'static,
        R: TypedReactive<T>,
    {
        let dispatcher = SingleKindDispatcher::<T, R>::new(reactor);
        run_reactor(self, region, config, dispatcher, Store::get)
    }
}

// ─── T6 multi-kind adapter + public surface ──────────────────────────────────

use crate::event::sourcing::MultiReactive;

/// Internal adapter that turns a `MultiReactive<Input>` reactor into a
/// `ReactorDispatcher<Input::Payload>`. It simply forwards the call — the
/// derive generates all the per-kind dispatch logic inside the reactor's
/// own `dispatch` method.
pub(crate) struct MultiKindDispatcher<Input, R>
where
    Input: crate::event::ProjectionInput,
    R: MultiReactive<Input>,
{
    reactor: R,
    _marker: std::marker::PhantomData<fn(Input) -> Input>,
}

impl<Input, R> MultiKindDispatcher<Input, R>
where
    Input: crate::event::ProjectionInput,
    R: MultiReactive<Input>,
{
    pub(crate) fn new(reactor: R) -> Self {
        Self {
            reactor,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<Input, R> ReactorDispatcher<Input::Payload> for MultiKindDispatcher<Input, R>
where
    Input: crate::event::ProjectionInput,
    R: MultiReactive<Input>,
{
    type Error = R::Error;

    fn dispatch(
        &mut self,
        event: &StoredEvent<Input::Payload>,
        out: &mut ReactionBatch,
    ) -> Result<(), ReactorStepError<Self::Error>> {
        // Forward — the derive-generated `dispatch` body returns
        // `Ok(())` for wrong-kind and matched-success, and
        // `Err(MultiDispatchError::{User, Decode})` for the two failure
        // modes (identical contract to T4b).
        match self.reactor.dispatch(event, out) {
            Ok(()) => Ok(()),
            Err(crate::event::sourcing::MultiDispatchError::User(e)) => {
                Err(ReactorStepError::User(e))
            }
            Err(crate::event::sourcing::MultiDispatchError::Decode(e)) => {
                Err(ReactorStepError::Decode(e))
            }
        }
    }
}

impl Store<Open> {
    /// Spawn a multi-event typed reactor loop over the JSON replay lane.
    ///
    /// Sibling of [`react_loop_typed`](Self::react_loop_typed) for reactors
    /// bound to multiple payload types. The reactor implements
    /// `MultiReactive<JsonValueInput>` (typically via `#[derive(MultiEventReactor)]`
    /// with `#[batpak(input = JsonValueInput)]`). The caller still supplies
    /// the [`Region`] explicitly; wrong-kind events inside that region are
    /// filtered by the derive-generated dispatch body.
    ///
    /// See [`react_loop_typed`](Self::react_loop_typed) for semantics — the
    /// two surfaces share the same underlying canal runner (ADR-0011) and
    /// the same decode-failure contract.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the background cursor-worker thread
    /// cannot be spawned.
    pub fn react_loop_multi<R>(
        self: &Arc<Self>,
        region: &Region,
        config: ReactorConfig,
        reactor: R,
    ) -> Result<TypedReactorHandle<R::Error>, StoreError>
    where
        R: MultiReactive<crate::event::JsonValueInput>,
    {
        let dispatcher = MultiKindDispatcher::<crate::event::JsonValueInput, R>::new(reactor);
        run_reactor(self, region, config, dispatcher, Store::get)
    }

    /// Spawn a multi-event typed reactor loop over the raw-msgpack replay
    /// lane. Events are delivered with payloads left as [`Vec<u8>`] so the
    /// reactor's `dispatch` (generated by `#[derive(MultiEventReactor)]`
    /// with `#[batpak(input = RawMsgpackInput)]`) can decode each kind via
    /// the raw-msgpack `DecodeTyped` impl.
    ///
    /// # Errors
    /// Returns [`StoreError::Io`] if the background cursor-worker thread
    /// cannot be spawned.
    pub fn react_loop_multi_raw<R>(
        self: &Arc<Self>,
        region: &Region,
        config: ReactorConfig,
        reactor: R,
    ) -> Result<TypedReactorHandle<R::Error>, StoreError>
    where
        R: MultiReactive<crate::event::RawMsgpackInput>,
    {
        let dispatcher = MultiKindDispatcher::<crate::event::RawMsgpackInput, R>::new(reactor);
        run_reactor(self, region, config, dispatcher, Store::get_raw)
    }
}
