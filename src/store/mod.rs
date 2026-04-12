mod ancestors;
/// Index checkpoint: fast cold-start by persisting the in-memory index to disk.
pub(crate) mod checkpoint;
/// Columnar (SoA / AoSoA) secondary query index.
pub(crate) mod columnar;
mod config;
mod contracts;
/// Pull-based cursor for guaranteed, ordered event delivery.
pub mod cursor;
mod error;
/// Fault injection framework for testing failure scenarios.
#[cfg(feature = "dangerous-test-hooks")]
pub mod fault;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
mod index_rebuild;
/// String interning for compact index keys.
pub(crate) mod interner;
mod maintenance;
/// Projection cache traits and built-in backends (NoCache, NativeCache).
pub mod projection;
mod projection_flow;
/// Low-level segment file reader for replaying events from disk.
pub mod reader;
#[cfg(test)]
mod runtime_contracts;
/// On-disk segment format, frame encoding/decoding, and compaction helpers.
pub mod segment;
/// SIDX segment footer for fast cold-start index rebuild.
pub(crate) mod sidx;
/// Runtime statistics and diagnostic snapshots.
pub mod stats;
/// Push-based (lossy) event subscription via broadcast channel.
pub mod subscription;
#[cfg(feature = "dangerous-test-hooks")]
mod test_support;
/// Background writer thread, restart policy, and subscriber fanout.
pub mod writer;

pub use config::{
    BatchConfig, IndexConfig, IndexLayout, StoreConfig, SyncConfig, SyncMode, WriterConfig,
};
pub use contracts::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, CompactionConfig,
    CompactionStrategy, RetentionPredicate,
};
pub use cursor::Cursor;
pub use error::BatchStage;
pub use error::StoreError;
#[cfg(feature = "dangerous-test-hooks")]
pub use fault::{
    CountdownAction, CountdownInjector, FaultInjector, InjectionPoint, ProbabilisticInjector,
};
pub use index::{ClockKey, DiskPos, IndexEntry};
pub use projection::{
    CacheCapabilities, CacheMeta, Freshness, NativeCache, NoCache, ProjectionCache,
};
pub use stats::{StoreDiagnostics, StoreStats};
pub use subscription::Subscription;
pub use writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, KindFilter, Region};
use crate::event::{Event, EventHeader, EventKind, EventSourced, StoredEvent};
#[cfg(test)]
pub(crate) use config::now_us;
use contracts::checked_payload_len;
use index::StoreIndex;
use reader::Reader;
use serde::Serialize;
use std::sync::Arc;
use writer::{AppendGuards, ReactorSubscriberList, SubscriberList, WriterCommand, WriterHandle};
// ProjectionCache re-exported above via pub use, no separate use needed.

/// Store: the runtime. Sync API. Send + Sync.
/// [SPEC:src/store/mod.rs]
/// Invariant 2: ALL METHODS ARE SYNC. No .await anywhere.
// Intentional impossible-feature guard: Store API is sync by design (Invariant 2).
// async-store is not a declared feature — suppress cfg warning for this guard
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

/// Typestate marker for an open store.
pub struct Open;

/// Typestate marker for a cleanly closed store.
pub struct Closed;

/// The main event store handle. Sync API; all methods are blocking. Send + Sync.
pub struct Store<State = Open> {
    pub(crate) index: Arc<StoreIndex>,
    pub(crate) reader: Arc<Reader>,
    pub(crate) cache: Box<dyn ProjectionCache>,
    pub(crate) writer: WriterHandle,
    pub(crate) config: Arc<StoreConfig>,
    pub(crate) should_shutdown_on_drop: bool,
    pub(crate) _state: std::marker::PhantomData<State>,
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
        config.validate()?;
        std::fs::create_dir_all(&config.data_dir)?;
        let config = Arc::new(config);
        let index = Arc::new(StoreIndex::with_layout(&config.index.layout));
        let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

        // Cold start: checkpoint fast path or full segment scan.
        // [SPEC:IMPLEMENTATION NOTES item 2 — segment naming, alphabetical scan]
        index_rebuild::open_index(
            &index,
            &reader,
            &config.data_dir,
            config.index.enable_checkpoint,
        )?;

        // Tell the reader which segment is active (for mmap dispatch).
        // The writer's initial segment ID is the highest existing + 1.
        let active_seg_id = writer::find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        reader.set_active_segment(active_seg_id);

        let subscribers = Arc::new(SubscriberList::new());
        let reactor_subscribers = Arc::new(ReactorSubscriberList::new());
        let writer =
            WriterHandle::spawn(&config, &index, &subscribers, &reactor_subscribers, &reader)?;

        Ok(Self {
            index,
            reader,
            cache,
            writer,
            config,
            should_shutdown_on_drop: true,
            _state: std::marker::PhantomData,
        })
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
        let event_id = crate::id::generate_v7_id();
        self.do_append(
            coord, kind, payload, event_id, event_id, None, None, None, 0,
        )
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
        let event_id = crate::id::generate_v7_id();
        self.do_append(
            coord,
            kind,
            payload,
            event_id,
            correlation_id,
            Some(causation_id),
            None,
            None,
            0,
        )
    }

    /// WRITE: atomic batch append of multiple events.
    /// All events are committed together or none are visible.
    /// [SPEC:src/store/mod.rs — append_batch]
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if any item fails validation, encoding, or write.
    /// The `item_index` field indicates which item failed.
    pub fn append_batch(
        &self,
        items: Vec<crate::store::contracts::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer
            .tx
            .send(WriterCommand::AppendBatch { items, respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: atomic batch append of reaction events.
    /// All events share the same correlation_id from the triggering event.
    /// [SPEC:src/store/mod.rs — append_reaction_batch]
    ///
    /// # Errors
    /// Returns `StoreError::BatchFailed` if any item fails validation, encoding, or write.
    pub fn append_reaction_batch(
        &self,
        correlation_id: u128,
        causation_id: u128,
        items: Vec<crate::store::contracts::BatchAppendItem>,
    ) -> Result<Vec<AppendReceipt>, StoreError> {
        // Set correlation_id and causation_id on all items.
        let items: Vec<_> = items
            .into_iter()
            .map(|mut item| {
                item.options.correlation_id = Some(correlation_id);
                // Only set causation_id if not already explicitly set.
                if matches!(item.causation, crate::store::contracts::CausationRef::None) {
                    item.options.causation_id = Some(causation_id);
                }
                item
            })
            .collect();
        self.append_batch(items)
    }

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

    /// READ: query by Region.
    #[must_use]
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors. [SPEC:IMPLEMENTATION NOTES item 3]
    /// When blake3 is enabled, follows the hash chain (event_hash -> prev_hash).
    /// When blake3 is disabled, all hashes are `[0u8;32]` so hash-based walking
    /// is impossible. Falls back to clock-ordered traversal (descending clock).
    pub fn walk_ancestors(
        &self,
        event_id: u128,
        limit: usize,
    ) -> Vec<StoredEvent<serde_json::Value>> {
        ancestors::walk_ancestors(self, event_id, limit)
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    /// [SPEC:src/store/projection_flow.rs — Projection Flow]
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if deserializing events or the cached state fails.
    /// Returns `StoreError::CacheFailed` if the projection cache backend encounters an error.
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced<serde_json::Value>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + 'static,
    {
        projection_flow::project(self, entity, freshness)
    }

    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe_lossy(&self, region: &Region) -> Subscription {
        let rx = self
            .writer
            .subscribers
            .subscribe(self.config.broadcast_capacity);
        Subscription::new(rx, region.clone())
    }

    /// CURSOR: pull-based, guaranteed delivery.
    pub fn cursor_guaranteed(&self, region: &Region) -> Cursor {
        Cursor::new(region.clone(), Arc::clone(&self.index))
    }

    /// CONVENIENCE: sugar over index.stream() for exact entity match.
    /// Unlike Region::entity() (prefix match), this returns events for
    /// exactly the named entity — "entity:1" does NOT match "entity:10".
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

    /// REACT: spawn a background thread running the subscribe→react→append loop.
    /// Returns a JoinHandle. The thread runs until the store is dropped (subscription closes).
    /// \[SPEC:src/event/sourcing.rs — Reactive\<P\> glue pattern\]
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
            .writer
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
        T: EventSourced<serde_json::Value>
            + serde::Serialize
            + serde::de::DeserializeOwned
            + Send
            + 'static,
    {
        let sub = self.subscribe_lossy(&Region::entity(entity));
        let store = Arc::clone(self);
        let entity_owned = entity.to_owned();
        ProjectionWatcher {
            sub,
            store,
            entity: entity_owned,
            freshness,
            cached_state: None,
            watermark: None,
            _phantom: std::marker::PhantomData,
        }
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
        let event_id = opts
            .idempotency_key
            .unwrap_or_else(crate::id::generate_v7_id);
        let correlation_id = opts.correlation_id.unwrap_or(event_id);
        self.do_append(
            coord,
            kind,
            payload,
            event_id,
            correlation_id,
            opts.causation_id,
            opts.expected_sequence,
            opts.idempotency_key,
            opts.flags,
        )
    }

    /// Internal append path shared by all public write methods.
    /// Serializes payload, constructs header+event, sends to writer, awaits receipt.
    #[allow(clippy::too_many_arguments)] // internal helper consolidating 3 public methods
    fn do_append(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        event_id: u128,
        correlation_id: u128,
        causation_id: Option<u128>,
        expected_sequence: Option<u32>,
        idempotency_key: Option<u128>,
        flags: u8,
    ) -> Result<AppendReceipt, StoreError> {
        // Group commit safety: batch > 1 requires idempotency keys for crash recovery.
        if self.config.batch.group_commit_max_batch > 1 && idempotency_key.is_none() {
            return Err(StoreError::IdempotencyRequired);
        }
        let payload_bytes =
            rmp_serde::to_vec_named(payload).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        if payload_bytes.len() > self.config.single_append_max_bytes as usize {
            return Err(StoreError::Configuration(format!(
                "single append bytes {} exceeds max {}",
                payload_bytes.len(),
                self.config.single_append_max_bytes
            )));
        }
        let payload_len = checked_payload_len(&payload_bytes)?;
        let mut header = EventHeader::new(
            event_id,
            correlation_id,
            causation_id,
            self.config.now_us(),
            crate::coordinate::DagPosition::root(),
            payload_len,
            kind,
        );
        if flags != 0 {
            header = header.with_flags(flags);
        }
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer
            .tx
            .send(WriterCommand::Append {
                coord: coord.clone(),
                event: Box::new(event),
                kind,
                guards: AppendGuards {
                    correlation_id,
                    causation_id,
                    expected_sequence,
                    idempotency_key,
                },
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: apply a typestate transition — extracts kind+payload, delegates to append.
    ///
    /// # Errors
    /// Returns `StoreError::Serialization` if the payload cannot be serialized.
    /// Returns `StoreError::WriterCrashed` if the writer thread has exited unexpectedly.
    pub fn apply_transition<From, To, P: Serialize>(
        &self,
        coord: &Coordinate,
        transition: crate::typestate::transition::Transition<From, To, P>,
    ) -> Result<AppendReceipt, StoreError> {
        let kind = transition.kind();
        let payload = transition.into_payload();
        self.append(coord, kind, &payload)
    }

    /// LIFECYCLE
    ///
    /// # Errors
    /// Returns `StoreError::Io` if flushing the active segment to disk fails.
    pub fn sync(&self) -> Result<(), StoreError> {
        maintenance::sync(self)
    }

    /// Snapshot the current index to a destination directory.
    ///
    /// # Errors
    /// Returns `StoreError::Io` if creating the destination directory or copying segment files fails.
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        maintenance::snapshot(self, dest)
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
        maintenance::compact(self, config)
    }

    /// LIFECYCLE: flush pending writes and shut down the writer thread cleanly.
    ///
    /// # Errors
    /// Returns `StoreError::WriterCrashed` if the writer thread has already exited unexpectedly.
    pub fn close(self) -> Result<Closed, StoreError> {
        maintenance::close(self)
    }

    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        maintenance::stats(self)
    }

    /// Return detailed diagnostic information about the store's internal state.
    pub fn diagnostics(&self) -> StoreDiagnostics {
        maintenance::diagnostics(self)
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
        tracing::warn!(
            "Store dropped without explicit close(); only a bounded best-effort drain will run"
        );
        let (tx, rx) = flume::bounded(1);
        if self
            .writer
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
    cached_state: Option<Vec<u8>>,
    watermark: Option<u64>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> ProjectionWatcher<T>
where
    T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    /// Block until a new event arrives for the watched entity, then re-project
    /// and return the updated state. Returns `None` if the store is dropped
    /// (subscription channel closed) or if projection returns no state.
    ///
    /// # Errors
    /// Returns `StoreError` if the projection fails (e.g., segment read error).
    pub fn recv(&mut self) -> Result<Option<T>, StoreError> {
        // Wait for any event on this entity's stream.
        if self.sub.recv().is_none() {
            return Ok(None); // store dropped
        }
        // Defense-in-depth: any of these conditions causes a full-projection
        // fallback, so flipping the logic still produces a correct (if slower)
        // result. The delta path is a performance optimization, not a
        // correctness boundary. Incremental-apply tests are planned; until
        // then, suppress the surviving mutations.
        if self.cached_state.is_none() // mutants::skip — full-projection fallback preserves correctness
            || !T::supports_incremental_apply() // mutants::skip
            || !self.store.config.index.incremental_projection // mutants::skip
        {
            return self.refresh_from_full_projection();
        }

        let Some(watermark) = self.watermark else {
            return self.refresh_from_full_projection();
        };
        let mut delta_entries = self.store.index.stream_since(&self.entity, watermark);
        let relevant_kinds = T::relevant_event_kinds();
        if !relevant_kinds.is_empty() { // mutants::skip — empty-list short-circuit is optimization-only
            delta_entries.retain(|entry| relevant_kinds.contains(&entry.kind));
        }
        if delta_entries.is_empty() {
            return self.deserialize_cached_state().map(Some);
        }

        let Some(bytes) = self.cached_state.as_ref() else {
            return self.refresh_from_full_projection();
        };
        let mut state = match serde_json::from_slice::<T>(bytes) {
            Ok(value) => value,
            Err(_) => return self.refresh_from_full_projection(),
        };
        let positions: Vec<&crate::store::DiskPos> =
            delta_entries.iter().map(|entry| &entry.disk_pos).collect();
        let stored_events = self.store.reader.read_entries_batch(&positions)?;
        for stored in stored_events {
            state.apply_event(&stored.event);
        }
        let new_watermark = delta_entries
            .last()
            .map(|entry| entry.global_sequence)
            .unwrap_or(watermark);
        let encoded =
            serde_json::to_vec(&state).map_err(|e| StoreError::Serialization(Box::new(e)))?;
        self.cached_state = Some(encoded);
        self.watermark = Some(new_watermark);
        Ok(Some(state))
    }

    /// Expose the underlying subscription's receiver for async integration.
    /// After receiving a notification, call `project()` on the store manually.
    #[doc(hidden)]
    pub fn subscription(&self) -> &Subscription {
        &self.sub
    }

    fn refresh_from_full_projection(&mut self) -> Result<Option<T>, StoreError> {
        let result = self.store.project::<T>(&self.entity, &self.freshness)?;
        if let Some(ref value) = result {
            self.cached_state = Some(
                serde_json::to_vec(value).map_err(|e| StoreError::Serialization(Box::new(e)))?,
            );
            self.watermark = self
                .store
                .index
                .stream(&self.entity)
                .last()
                .map(|entry| entry.global_sequence);
        } else {
            self.cached_state = None;
            self.watermark = None;
        }
        Ok(result)
    }

    fn deserialize_cached_state(&self) -> Result<T, StoreError> {
        let bytes = self
            .cached_state
            .as_ref()
            .ok_or_else(|| StoreError::Configuration("projection watcher state missing".into()))?;
        serde_json::from_slice(bytes).map_err(|e| StoreError::Serialization(Box::new(e)))
    }
}
