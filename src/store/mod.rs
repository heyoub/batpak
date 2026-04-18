mod ancestry;
mod append;
pub(crate) mod cold_start;
mod config;
/// Push subscriptions (lossy) and pull cursors (guaranteed) for event delivery.
pub mod delivery;
mod error;
/// Fault injection framework for testing failure scenarios.
#[cfg(feature = "dangerous-test-hooks")]
pub mod fault;
mod hidden_ranges;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
mod lifecycle;
/// Projection cache traits and built-in backends (NoCache, NativeCache).
pub mod projection;
/// Typed reactor output batch — accumulator handed to typed reactor handlers.
pub mod reaction;
/// Typed reactor public surface + shared internal canal runner.
pub mod reactor_typed;
#[cfg(test)]
mod runtime_contracts;
/// On-disk segment format, frame encoding/decoding, and compaction helpers.
pub mod segment;
/// Runtime statistics and diagnostic snapshots.
pub mod stats;
#[cfg(feature = "dangerous-test-hooks")]
mod test_support;
pub(crate) mod write;

pub use append::{
    AppendOptions, AppendPositionHint, AppendReceipt, BatchAppendItem, CausationRef,
    CompactionConfig, CompactionStrategy, RetentionPredicate,
};
pub use cold_start::rebuild::{OpenIndexPath, OpenIndexReport};
pub use config::{
    BatchConfig, IndexConfig, IndexTopology, StoreConfig, SyncConfig, SyncMode, WriterConfig,
};
pub use delivery::cursor::{Cursor, CursorWorkerAction, CursorWorkerConfig, CursorWorkerHandle};
pub use delivery::subscription::Subscription;
pub use error::StoreError;
#[cfg(feature = "dangerous-test-hooks")]
pub use fault::{
    CountdownAction, CountdownInjector, FaultInjector, InjectionPoint, ProbabilisticInjector,
};
pub use index::{ClockKey, DiskPos, IndexEntry};
pub use projection::watch::ProjectionWatcher;
pub use projection::{
    CacheCapabilities, CacheMeta, Freshness, NativeCache, NoCache, ProjectionCache,
};
pub use reaction::ReactionBatch;
pub use reactor_typed::{ReactorConfig, ReactorError, TypedReactorHandle};
pub use stats::{StoreDiagnostics, StoreStats, WriterPressure};
pub use write::control::{AppendTicket, BatchAppendTicket, Outbox, VisibilityFence};
pub use write::writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, KindFilter, Region};
use crate::event::{EventKind, EventPayload, EventSourced, StoredEvent};
#[cfg(test)]
pub(crate) use config::now_us;
use index::StoreIndex;
use segment::scan::Reader;
use serde::Serialize;
use std::sync::Arc;
use write::control::AppendSubmission;
use write::fanout::{ReactorSubscriberList, SubscriberList};
use write::writer::{WriterCommand, WriterHandle};
// ProjectionCache re-exported above via pub use, no separate use needed.

/// Store: the runtime. Sync API. Send + Sync.
/// Invariant 2: all methods are sync; async integration lives in channels.
// justifies: async-store is not a declared feature; this compile_error guard must survive cargo check by silencing the unexpected cfg name
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

/// Typestate marker for an open store.
pub struct Open;

/// Typestate marker for a cleanly closed store.
pub struct Closed;

/// Typestate marker for a read-only store handle.
pub struct ReadOnly;

/// The main event store handle. Sync API; all methods are blocking. Send + Sync.
pub struct Store<State = Open> {
    pub(crate) index: Arc<StoreIndex>,
    pub(crate) reader: Arc<Reader>,
    pub(crate) cache: Box<dyn ProjectionCache>,
    pub(crate) writer: Option<WriterHandle>,
    pub(crate) config: Arc<StoreConfig>,
    pub(crate) runtime: Arc<config::ValidatedStoreConfig>,
    pub(crate) should_shutdown_on_drop: bool,
    pub(crate) open_report: Option<cold_start::rebuild::OpenIndexReport>,
    pub(crate) _state: std::marker::PhantomData<State>,
}

struct OpenComponents {
    runtime: Arc<config::ValidatedStoreConfig>,
    config: Arc<StoreConfig>,
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    open_report: cold_start::rebuild::OpenIndexReport,
}

fn open_components(config: StoreConfig) -> Result<OpenComponents, StoreError> {
    let runtime = Arc::new(config.validated()?);
    std::fs::create_dir_all(&config.data_dir)?;
    let config = Arc::new(config);
    let index = Arc::new(StoreIndex::with_config(&config.index));
    let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

    // Cold start: checkpoint/mmap fast paths or full segment scan.
    // Segment files are named so lexicographic order matches replay order.
    let open_report =
        cold_start::rebuild::open_index(&index, &reader, &config.data_dir, runtime.cold_start)?;

    // Tell the reader which segment is active (for mmap dispatch).
    // The writer's initial segment ID is the highest existing + 1.
    let active_seg_id = write::writer::find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
    reader.set_active_segment(active_seg_id);

    Ok(OpenComponents {
        runtime,
        config,
        index,
        reader,
        open_report,
    })
}

impl Store<Open> {
    /// Open a store at the given config's data directory. Creates the directory if absent.
    /// Uses `NoCache` for projection (no external cache backend).
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NoCache))
    }

    /// Open a store with the built-in file-backed projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the cache directory cannot be created,
    /// or any error from [`Store::open_with_cache`].
    pub fn open_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open a store with a custom projection cache backend.
    /// Use [`NativeCache`] for file-backed cache-accelerated `project()` calls.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
        } = open_components(config)?;

        let subscribers = Arc::new(SubscriberList::new());
        let reactor_subscribers = Arc::new(ReactorSubscriberList::new());
        let writer = WriterHandle::spawn(
            &config,
            &runtime,
            &index,
            &subscribers,
            &reactor_subscribers,
            &reader,
        )?;

        Ok(Self {
            index,
            reader,
            cache,
            writer: Some(writer),
            config,
            runtime,
            should_shutdown_on_drop: true,
            open_report: Some(open_report),
            _state: std::marker::PhantomData,
        })
    }

    /// Build a producer-side outbox for staged batch submission.
    pub fn outbox(&self) -> Outbox<'_> {
        Outbox::new(self, None)
    }

    /// Begin a public visibility fence. Only one fence may be active at a time.
    ///
    /// # Errors
    /// Returns an error if another public visibility fence is already active or
    /// if the writer cannot acknowledge the new fence.
    pub fn begin_visibility_fence(&self) -> Result<VisibilityFence<'_>, StoreError> {
        let token = self.index.begin_visibility_fence()?;
        let (tx, rx) = flume::bounded(1);
        let send_result = self
            .writer_handle()?
            .tx
            .send(WriterCommand::BeginVisibilityFence { token, respond: tx });
        if send_result.is_err() {
            let _ = self.index.cancel_visibility_fence(token);
            return Err(StoreError::WriterCrashed);
        }
        rx.recv().map_err(|_| StoreError::WriterCrashed)??;
        Ok(VisibilityFence::new(self, token))
    }

    /// Snapshot the current writer mailbox pressure.
    pub fn writer_pressure(&self) -> WriterPressure {
        let writer = self.writer_ref();
        WriterPressure {
            queue_len: writer.tx.len(),
            capacity: self.config.writer.channel_capacity,
        }
    }

    /// Nonblocking root-cause append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the append for background execution.
    pub fn submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(coord, kind, payload, AppendSubmission::root())
    }

    /// Nonblocking reaction append submission.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced while
    /// staging the reaction append for background execution.
    pub fn submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_prepared(
            coord,
            kind,
            payload,
            AppendSubmission::reaction(correlation_id, causation_id),
        )
    }

    /// Nonblocking batch append submission.
    ///
    /// Every item's coordinate is revalidated synchronously at this entry so
    /// that invalid coordinates surface to the caller rather than being
    /// deferred to the writer thread.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidCoordinate`] if any item's coordinate
    /// fails validation, or any enqueue or writer error surfaced while
    /// staging the batch for background execution.
    pub fn submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<BatchAppendTicket, StoreError> {
        self.ensure_no_active_public_fence()?;
        for (i, item) in items.iter().enumerate() {
            if let Err(err) = item.coord().validate() {
                return Err(StoreError::InvalidCoordinate {
                    index: Some(i),
                    reason: format!("{err}"),
                });
            }
        }
        self.submit_batch_with_fence_impl(items, None)
    }

    /// Attempt a root-cause submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit(coord, kind, payload)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a reaction submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error surfaced when the
    /// operation proceeds past the soft-pressure gate.
    pub fn try_submit_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate() {
            return Ok(outcome);
        }
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)
            .map(crate::outcome::Outcome::ok)
    }

    /// Attempt a batch submission without blocking if the writer is under pressure.
    ///
    /// # Errors
    /// Returns any enqueue or writer error surfaced when the operation
    /// proceeds past the soft-pressure gate.
    pub fn try_submit_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<crate::outcome::Outcome<BatchAppendTicket>, StoreError> {
        if self.index.active_visibility_fence().is_some() {
            return Ok(crate::outcome::Outcome::cancelled(
                "visibility fence is active; submit through the fence",
            ));
        }
        if let Some(outcome) = self.submit_pressure_gate_batch() {
            return Ok(outcome);
        }
        self.submit_batch(items).map(crate::outcome::Outcome::ok)
    }

    /// WRITE: append a new root-cause event.
    /// correlation_id defaults to event_id (self-correlated). causation_id = None.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append",
            entity = coord.entity(),
            scope = coord.scope(),
            event_kind = kind.type_id()
        );
        self.submit(coord, kind, payload)?.wait()
    }

    /// WRITE: append a reaction (caused by another event).
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_reaction",
            entity = coord.entity(),
            scope = coord.scope(),
            correlation_id = format_args!("{correlation_id:032x}"),
            causation_id = format_args!("{causation_id:032x}")
        );
        self.submit_reaction(coord, kind, payload, correlation_id, causation_id)?
            .wait()
    }

    /// WRITE: atomic batch append of multiple events.
    /// All events are committed together or none are visible.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if any item fails validation, encoding, durability,
    /// or publish preparation. The `item_index` field indicates which item failed.
    pub fn append_batch(
        &self,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        self.submit_batch(items)?.wait()
    }

    /// WRITE: atomic batch append of reaction events.
    /// All events share the same correlation_id from the triggering event.
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if any item fails validation, encoding, durability,
    /// or publish preparation.
    pub fn append_reaction_batch(
        &self,
        correlation_id: u128,
        causation_id: u128,
        items: Vec<crate::store::append::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // Set correlation_id and causation_id on all items.
        let items: Vec<_> = items
            .into_iter()
            .map(|item| {
                let mut options = item.options();
                options.correlation_id = Some(correlation_id);
                // Only set causation_id if not already explicitly set.
                if item.causation().uses_options_fallback() {
                    options.causation_id = Some(causation_id);
                }
                item.with_options(options)
            })
            .collect();
        self.append_batch(items)
    }

    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe_lossy(&self, region: &Region) -> Subscription {
        // justifies: Store<Open> typestate guarantees writer presence at
        // construction (see Store::open_with_cache — it fails the open
        // instead of yielding Store<Open> if the writer cannot be spawned).
        // The expect here documents an invariant, it does not recover from
        // one: observing None means the store is mid-drop and every public
        // path through Store<Open> is already invalid.
        let rx = self
            .writer_ref()
            .subscribers
            .subscribe(self.config.broadcast_capacity);
        Subscription::new(rx, region.clone())
    }

    /// Crate-private accessor that encodes the `Store<Open>` typestate
    /// invariant: an `Open` store always holds a writer handle.
    ///
    /// Panics if the invariant is violated — which only happens when a
    /// `Store<Open>` has been partially moved out of during drop, a context
    /// in which every public method is already unreachable.
    pub(crate) fn writer_ref(&self) -> &WriterHandle {
        // justifies: typestate invariant of Store<Open> — see open_components
        // and Store::open_with_cache for the construction guarantee.
        self.writer
            .as_ref()
            .expect("invariant: Store<Open> is constructed with a writer handle")
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
            .subscribe(self.config.broadcast_capacity);
        std::thread::Builder::new()
            .name("batpak-reactor".into())
            .spawn(move || {
                while let Ok(envelope) = sub.recv() {
                    let notif = envelope.notification;
                    if !region.matches_event(notif.coord.entity(), notif.coord.scope(), notif.kind)
                    {
                        continue;
                    }
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
    /// that emits an updated projection `T` whenever new events arrive for `entity`.
    ///
    /// Internally subscribes to entity events, then re-projects on each notification.
    /// The watcher is pull-based: the caller drives the loop via `watcher.recv()`.
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
        let sub = self.subscribe_lossy(&Region::entity(entity));
        let store = Arc::clone(self);
        let entity_owned = entity.to_owned();
        ProjectionWatcher::new(
            sub,
            store,
            entity_owned,
            freshness,
            self.entity_generation(entity).unwrap_or(0),
        )
    }

    /// WRITE: append with CAS, idempotency, custom correlation/causation.
    /// CAS and idempotency checks execute inside the writer thread under
    /// the entity lock — no TOCTOU race between check and commit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::SequenceMismatch` if the expected sequence does not match.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_with_options(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        tracing::debug!(
            target: "batpak::flow",
            flow = "append_with_options",
            entity = coord.entity(),
            scope = coord.scope(),
            has_cas = opts.expected_sequence.is_some(),
            has_idempotency = opts.idempotency_key.is_some()
        );
        self.submit_prepared(coord, kind, payload, AppendSubmission::with_options(opts))?
            .wait()
    }

    /// WRITE: apply a typestate transition — extracts kind+payload, delegates to append.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn apply_transition<
        From: crate::typestate::transition::StateMarker,
        To: crate::typestate::transition::StateMarker,
        P: Serialize,
    >(
        &self,
        coord: &Coordinate,
        transition: crate::typestate::transition::Transition<From, To, P>,
    ) -> Result<AppendReceipt, StoreError> {
        let kind = transition.kind();
        let payload = transition.into_payload();
        self.append(coord, kind, &payload)
    }

    /// WRITE (typed): append a root-cause event — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendReceipt, StoreError> {
        self.append(coord, T::KIND, payload)
    }

    /// WRITE (typed): append with options — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_typed_with_options<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_with_options(coord, T::KIND, payload, opts)
    }

    /// WRITE (typed): nonblocking submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<AppendTicket, StoreError> {
        self.submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): attempt submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit(coord, T::KIND, payload)
    }

    /// WRITE (typed): append a reaction — kind derived from `T::KIND`.
    ///
    /// `correlation_id` and `causation_id` are still supplied explicitly;
    /// only the `kind` becomes implicit.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn append_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        self.append_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): nonblocking reaction submit — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendTicket, StoreError> {
        self.submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// WRITE (typed): attempt reaction submit without blocking under pressure — kind derived from `T::KIND`.
    ///
    /// # Errors
    /// Returns any serialization, enqueue, or writer error.
    pub fn try_submit_reaction_typed<T: EventPayload>(
        &self,
        coord: &Coordinate,
        payload: &T,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<crate::outcome::Outcome<AppendTicket>, StoreError> {
        self.try_submit_reaction(coord, T::KIND, payload, correlation_id, causation_id)
    }

    /// LIFECYCLE
    ///
    /// # Errors
    /// Returns `StoreError::Io` if flushing the active segment to disk fails.
    pub fn sync(&self) -> Result<(), StoreError> {
        lifecycle::sync(self)
    }

    /// Snapshot the current index to a destination directory.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        lifecycle::snapshot(self, dest)
    }

    /// Compact: merge sealed segments, optionally filtering events.
    /// Returns the number of segments removed and bytes reclaimed.
    /// The active (currently-written) segment is never touched.
    ///
    /// **IMPORTANT**: compact() rebuilds the in-memory index from disk.
    /// Appends that arrive during compaction are safe (they go to the active
    /// segment which is not compacted), but the index rebuild syncs the writer
    /// before and after to minimize the window for stale index state.
    /// For maximum safety, avoid high-throughput appends during compaction.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if reading, writing, or removing segment files fails.
    pub fn compact(
        &self,
        config: &CompactionConfig,
    ) -> Result<segment::CompactionResult, StoreError> {
        lifecycle::compact(self, config)
    }

    /// LIFECYCLE: flush pending writes and shut down the writer thread cleanly.
    ///
    /// # Errors
    /// Returns `StoreError::WriterCrashed` if the writer thread has already exited unexpectedly.
    pub fn close(self) -> Result<Closed, StoreError> {
        lifecycle::close(self)
    }
}

impl Store<ReadOnly> {
    /// Open the store without starting a writer thread.
    ///
    /// # Errors
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NoCache))
    }

    /// Open the store in read-only mode with the built-in projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the native cache cannot be
    /// opened, or any error returned by [`Store::open_read_only_with_cache`].
    pub fn open_read_only_with_native_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_read_only_with_cache(config, Box::new(NativeCache::open(cache_path)?))
    }

    /// Open the store in read-only mode with a custom projection cache backend.
    ///
    /// # Errors
    /// Returns any configuration, directory-creation, or cold-start rebuild
    /// error surfaced while opening the store in read-only mode.
    pub fn open_read_only_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        let OpenComponents {
            runtime,
            config,
            index,
            reader,
            open_report,
        } = open_components(config)?;

        Ok(Self {
            index,
            reader,
            cache,
            writer: None,
            config,
            runtime,
            should_shutdown_on_drop: false,
            open_report: Some(open_report),
            _state: std::marker::PhantomData,
        })
    }
}

impl<State> Store<State> {
    /// READ: get a single event by ID.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading from disk fails.
    pub fn get(&self, event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let entry = self
            .index
            .get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: fetch a single event by ID with the payload left as raw
    /// MessagePack bytes. Mirrors [`get`](Self::get) but skips the
    /// JSON-decode step, suitable for the `RawMsgpackInput` lane of a
    /// multi-event reactor.
    ///
    /// # Errors
    /// Returns `StoreError::NotFound` if no event with that ID exists.
    /// Returns `StoreError::Io` or `StoreError::Serialization` if reading
    /// from disk fails.
    pub fn get_raw(&self, event_id: u128) -> Result<StoredEvent<Vec<u8>>, StoreError> {
        let entry = self
            .index
            .get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry_raw(&entry.disk_pos)
    }

    /// READ: query by Region.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors.
    pub fn walk_ancestors(
        &self,
        event_id: u128,
        limit: usize,
    ) -> Vec<StoredEvent<serde_json::Value>> {
        ancestry::walk_ancestors(self, event_id, limit)
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    ///
    /// # Errors
    /// Returns any replay, deserialization, cache, or disk-read error surfaced
    /// while reconstructing the projection state.
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project(self, entity, freshness)
    }

    /// Return the current per-entity generation if the entity exists.
    ///
    /// Generations advance monotonically on every insert for that entity.
    /// When entity-group overlays are disabled, this falls back to the entity
    /// stream length so callers still get a stable monotonic skip token.
    pub fn entity_generation(&self, entity: &str) -> Option<u64> {
        self.index.entity_generation(entity)
    }

    /// Project only when the entity changed since `last_seen_generation`.
    ///
    /// Returns `Ok(None)` when no change is observed. Otherwise returns the new
    /// generation together with the freshly projected state.
    ///
    /// # Errors
    /// Returns any error surfaced by [`Store::project`] when the entity has
    /// changed and the projection must be rebuilt.
    pub fn project_if_changed<T>(
        &self,
        entity: &str,
        last_seen_generation: u64,
        freshness: &Freshness,
    ) -> Result<Option<(u64, Option<T>)>, StoreError>
    where
        T: EventSourced + serde::Serialize + serde::de::DeserializeOwned + 'static,
        T::Input: projection::flow::ReplayInput,
    {
        projection::flow::project_if_changed(self, entity, last_seen_generation, freshness)
    }

    /// CONVENIENCE: sugar over index.stream() for exact entity match.
    #[must_use]
    pub fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.index.stream(entity)
    }

    /// READ: query all events in the given scope.
    #[must_use]
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }

    /// READ: query all events of the given event kind across all entities and scopes.
    #[must_use]
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// READ (typed): query all events whose kind matches `T::KIND`.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`.
    #[must_use]
    pub fn by_fact_typed<T: EventPayload>(&self) -> Vec<IndexEntry> {
        self.by_fact(T::KIND)
    }

    /// CURSOR: pull-based, guaranteed delivery.
    ///
    /// Available on both `Store<Open>` and `Store<ReadOnly>`. The cursor reads
    /// from the in-memory index and cannot lose events.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }

    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        lifecycle::stats(self)
    }

    /// Return detailed diagnostic information about the store's internal state.
    pub fn diagnostics(&self) -> StoreDiagnostics {
        lifecycle::diagnostics(self)
    }
}

/// Safety net: if Store is dropped without calling close(), send a best-effort
/// Shutdown to the writer thread and wait briefly for it to drain pending events.
/// close(self) is still the preferred explicit path for guaranteed clean shutdown.
impl<State> Drop for Store<State> {
    fn drop(&mut self) {
        if !self.should_shutdown_on_drop {
            return;
        }
        let Some(writer) = self.writer.as_ref() else {
            return;
        };
        tracing::warn!(
            "Store dropped without explicit close(); only a bounded best-effort drain will run"
        );
        let (tx, rx) = flume::bounded(1);
        if writer
            .tx
            .send(WriterCommand::Shutdown { respond: tx })
            .is_ok()
        {
            // Wait up to 100ms for the writer to drain pending events.
            // This prevents data loss when Store is dropped without close().
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
    }
}
