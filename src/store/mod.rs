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
#[cfg(feature = "test-support")]
pub mod fault;
/// In-memory 2D event index, rebuilt from segments on startup.
pub mod index;
mod index_rebuild;
/// String interning for compact index keys.
pub(crate) mod interner;
mod maintenance;
/// Projection cache traits and built-in backends (NoCache, redb, LMDB).
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
#[cfg(feature = "test-support")]
mod test_support;
/// Background writer thread, restart policy, and subscriber fanout.
pub mod writer;

pub use config::{IndexLayout, StoreConfig, SyncMode};
pub use contracts::{
    AppendOptions, AppendReceipt, BatchAppendItem, CausationRef, CompactionConfig,
    CompactionStrategy, RetentionPredicate,
};
pub use cursor::Cursor;
pub use error::BatchStage;
pub use error::StoreError;
#[cfg(feature = "test-support")]
pub use fault::{
    CountdownAction, CountdownInjector, FaultInjector, InjectionPoint, ProbabilisticInjector,
};
pub use index::{ClockKey, DiskPos, IndexEntry};
#[cfg(feature = "lmdb")]
pub use projection::LmdbCache;
#[cfg(feature = "redb")]
pub use projection::RedbCache;
pub use projection::{CacheCapabilities, CacheMeta, Freshness, NoCache, ProjectionCache};
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
use writer::{AppendGuards, SubscriberList, WriterCommand, WriterHandle};
// ProjectionCache re-exported above via pub use, no separate use needed.

/// Store: the runtime. Sync API. Send + Sync.
/// [SPEC:src/store/mod.rs]
/// Invariant 2: ALL METHODS ARE SYNC. No .await anywhere.
// Intentional impossible-feature guard: Store API is sync by design (Invariant 2).
// async-store is not a declared feature — suppress cfg warning for this guard
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

/// The main event store handle. Sync API; all methods are blocking. Send + Sync.
pub struct Store {
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    cache: Box<dyn ProjectionCache>,
    writer: WriterHandle,
    config: Arc<StoreConfig>,
}

impl Store {
    /// Open a store at the given config's data directory. Creates the directory if absent.
    /// Uses `NoCache` for projection (no external cache backend).
    ///
    /// # Errors
    /// Returns `StoreError::Io` if the data directory cannot be created or segments cannot be read.
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NoCache))
    }

    /// Open a store with the built-in redb projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the redb database cannot be opened,
    /// or any error from [`Store::open_with_cache`].
    #[cfg(feature = "redb")]
    pub fn open_with_redb_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(RedbCache::open(cache_path)?))
    }

    /// Open a store with the built-in LMDB projection cache.
    ///
    /// # Errors
    /// Returns [`StoreError::CacheFailed`] if the LMDB environment cannot be opened,
    /// or any error from [`Store::open_with_cache`].
    #[cfg(feature = "lmdb")]
    pub fn open_with_lmdb_cache(
        config: StoreConfig,
        cache_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, StoreError> {
        let map_size = config.cache_map_size_bytes;
        Self::open_with_cache(config, Box::new(LmdbCache::open(cache_path, map_size)?))
    }

    /// Open a store with a custom projection cache backend.
    /// Use `RedbCache` or `LmdbCache` (feature-gated) for cache-accelerated `project()` calls.
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
        let index = Arc::new(StoreIndex::with_layout(&config.index_layout));
        let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

        // Cold start: checkpoint fast path or full segment scan.
        // [SPEC:IMPLEMENTATION NOTES item 2 — segment naming, alphabetical scan]
        index_rebuild::open_index(&index, &reader, &config.data_dir, config.enable_checkpoint)?;

        // Tell the reader which segment is active (for mmap dispatch).
        // The writer's initial segment ID is the highest existing + 1.
        let active_seg_id = writer::find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        reader.set_active_segment(active_seg_id);

        let subscribers = Arc::new(SubscriberList::new());
        let writer = WriterHandle::spawn(&config, &index, &subscribers, &reader)?;

        Ok(Self {
            index,
            reader,
            cache,
            writer,
            config,
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
    pub fn subscribe(&self, region: &Region) -> Subscription {
        let rx = self
            .writer
            .subscribers
            .subscribe(self.config.broadcast_capacity);
        Subscription::new(rx, region.clone())
    }

    /// CURSOR: pull-based, guaranteed delivery.
    pub fn cursor(&self, region: &Region) -> Cursor {
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
        let sub = self.subscribe(region);
        std::thread::Builder::new()
            .name("batpak-reactor".into())
            .spawn(move || {
                while let Some(notif) = sub.recv() {
                    let stored = match store.get(notif.event_id) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(
                                "react_loop: failed to get event {}: {e}",
                                notif.event_id
                            );
                            continue;
                        }
                    };
                    for (coord, kind, payload) in reactor.react(&stored.event) {
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
        let sub = self.subscribe(&Region::entity(entity));
        let store = Arc::clone(self);
        let entity_owned = entity.to_owned();
        ProjectionWatcher {
            sub,
            store,
            entity: entity_owned,
            freshness,
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
        if self.config.group_commit_max_batch > 1 && idempotency_key.is_none() {
            return Err(StoreError::IdempotencyRequired);
        }
        let payload_bytes =
            rmp_serde::to_vec_named(payload).map_err(|e| StoreError::Serialization(Box::new(e)))?;
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
                entity: coord.entity_arc(),
                scope: coord.scope_arc(),
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
    pub fn close(self) -> Result<(), StoreError> {
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
impl Drop for Store {
    fn drop(&mut self) {
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
    store: Arc<Store>,
    entity: String,
    freshness: Freshness,
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
    pub fn recv(&self) -> Result<Option<T>, StoreError> {
        // Wait for any event on this entity's stream.
        if self.sub.recv().is_none() {
            return Ok(None); // store dropped
        }
        // Re-project with the latest state.
        self.store.project::<T>(&self.entity, &self.freshness)
    }

    /// Expose the underlying subscription's receiver for async integration.
    /// After receiving a notification, call `project()` on the store manually.
    #[doc(hidden)]
    pub fn subscription(&self) -> &Subscription {
        &self.sub
    }
}
