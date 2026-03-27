pub mod cursor;
pub mod index;
pub mod projection;
pub mod reader;
pub mod segment;
pub mod subscription;
pub mod writer;

pub use cursor::Cursor;
pub use index::{ClockKey, DiskPos, IndexEntry};
#[cfg(feature = "lmdb")]
pub use projection::LmdbCache;
#[cfg(feature = "redb")]
pub use projection::RedbCache;
pub use projection::{CacheMeta, Freshness, NoCache, ProjectionCache};
pub use subscription::Subscription;
pub use writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, CoordinateError, KindFilter, Region};
use crate::event::{Event, EventHeader, EventKind, EventSourced, StoredEvent};
use index::StoreIndex;
use reader::Reader;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use writer::{AppendGuards, SubscriberList, WriterCommand, WriterHandle};
// ProjectionCache re-exported above via pub use, no separate use needed.

/// Store: the runtime. Sync API. Send + Sync.
/// [SPEC:src/store/mod.rs]
/// Invariant 2: ALL METHODS ARE SYNC. No .await anywhere.
#[allow(unexpected_cfgs)]
#[cfg(feature = "async-store")]
compile_error!("INVARIANT 2: Store API is sync. Use spawn_blocking or flume recv_async.");

pub struct Store {
    index: Arc<StoreIndex>,
    reader: Arc<Reader>,
    cache: Box<dyn ProjectionCache>,
    writer: WriterHandle,
    config: Arc<StoreConfig>,
}

/// Sync strategy for segment fsync.
#[derive(Clone, Debug, Default)]
pub enum SyncMode {
    /// sync_all: syncs data + metadata (safest, slower)
    #[default]
    SyncAll,
    /// sync_data: syncs data only (faster, sufficient for most use cases)
    SyncData,
}

/// StoreConfig: all settings for a Store instance.
/// No Default — callers must provide data_dir via `StoreConfig::new(path)`.
/// Manual Clone and Debug impls because `clock` field is `Arc<dyn Fn>`.
pub struct StoreConfig {
    pub data_dir: PathBuf,
    pub segment_max_bytes: u64,
    pub sync_every_n_events: u32,
    pub fd_budget: usize,
    pub writer_channel_capacity: usize,
    pub broadcast_capacity: usize,
    pub cache_map_size_bytes: usize,
    pub restart_policy: RestartPolicy,
    pub shutdown_drain_limit: usize,
    /// Optional writer thread stack size. None = OS default (~8MB on Linux).
    pub writer_stack_size: Option<usize>,
    /// Injectable clock for deterministic testing. Returns microseconds since epoch.
    /// None = std::time::SystemTime::now() (production default).
    pub clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Sync mode: SyncAll (data+metadata, default) or SyncData (data only, faster).
    pub sync_mode: SyncMode,
}

impl StoreConfig {
    /// Create a StoreConfig with required data_dir and sensible defaults.
    /// All numeric defaults are documented. Override fields after construction
    /// to tune for your deployment (embedded, server, CLI).
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            segment_max_bytes: 256 * 1024 * 1024, // 256MB — tune down for embedded
            sync_every_n_events: 1000,            // durability vs throughput tradeoff
            fd_budget: 64,                        // LRU FD cache slots
            writer_channel_capacity: 4096,        // back-pressure threshold
            broadcast_capacity: 8192,             // per-subscriber lossy buffer
            cache_map_size_bytes: 64 * 1024 * 1024, // 64MB — used by LmdbCache
            restart_policy: RestartPolicy::default(),
            shutdown_drain_limit: 1024, // max queued commands drained on shutdown
            writer_stack_size: None,    // OS default (~8MB on Linux)
            clock: None,                // SystemTime::now() default
            sync_mode: SyncMode::default(), // SyncAll (safest)
        }
    }

    /// Get current timestamp in microseconds, using the injectable clock if set.
    pub(crate) fn now_us(&self) -> i64 {
        match &self.clock {
            Some(f) => f(),
            None => now_us(), // module-level fallback using SystemTime
        }
    }
}

impl Clone for StoreConfig {
    fn clone(&self) -> Self {
        Self {
            data_dir: self.data_dir.clone(),
            segment_max_bytes: self.segment_max_bytes,
            sync_every_n_events: self.sync_every_n_events,
            fd_budget: self.fd_budget,
            writer_channel_capacity: self.writer_channel_capacity,
            broadcast_capacity: self.broadcast_capacity,
            cache_map_size_bytes: self.cache_map_size_bytes,
            restart_policy: self.restart_policy.clone(),
            shutdown_drain_limit: self.shutdown_drain_limit,
            writer_stack_size: self.writer_stack_size,
            clock: self.clock.clone(),
            sync_mode: self.sync_mode.clone(),
        }
    }
}

impl std::fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConfig")
            .field("data_dir", &self.data_dir)
            .field("segment_max_bytes", &self.segment_max_bytes)
            .field("sync_every_n_events", &self.sync_every_n_events)
            .field("fd_budget", &self.fd_budget)
            .field("writer_channel_capacity", &self.writer_channel_capacity)
            .field("broadcast_capacity", &self.broadcast_capacity)
            .field("cache_map_size_bytes", &self.cache_map_size_bytes)
            .field("restart_policy", &self.restart_policy)
            .field("shutdown_drain_limit", &self.shutdown_drain_limit)
            .field("writer_stack_size", &self.writer_stack_size)
            .field("clock", &self.clock.as_ref().map(|_| "<fn>"))
            .field("sync_mode", &self.sync_mode)
            .finish()
    }
}

/// StoreError: every error the store can produce.
/// [SPEC:src/store/mod.rs — StoreError variants]
#[derive(Debug)]
#[non_exhaustive]
pub enum StoreError {
    Io(std::io::Error),
    Coordinate(CoordinateError),
    Serialization(String),
    CrcMismatch {
        segment_id: u64,
        offset: u64,
    },
    CorruptSegment {
        segment_id: u64,
        detail: String,
    },
    NotFound(u128),
    SequenceMismatch {
        entity: String,
        expected: u32,
        actual: u32,
    },
    DuplicateEvent(u128),
    WriterCrashed,
    ShuttingDown,
    CacheFailed(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Coordinate(e) => write!(f, "coordinate error: {e}"),
            Self::Serialization(s) => write!(f, "serialization error: {s}"),
            Self::CrcMismatch { segment_id, offset } => {
                write!(f, "CRC mismatch in segment {segment_id} at offset {offset}")
            }
            Self::CorruptSegment { segment_id, detail } => {
                write!(f, "corrupt segment {segment_id}: {detail}")
            }
            Self::NotFound(id) => write!(f, "event {id:032x} not found"),
            Self::SequenceMismatch {
                entity,
                expected,
                actual,
            } => write!(
                f,
                "CAS failed for {entity}: expected seq {expected}, got {actual}"
            ),
            Self::DuplicateEvent(key) => write!(f, "duplicate idempotency key {key:032x}"),
            Self::WriterCrashed => write!(f, "writer thread crashed"),
            Self::ShuttingDown => write!(f, "store is shutting down"),
            Self::CacheFailed(s) => write!(f, "cache error: {s}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<CoordinateError> for StoreError {
    fn from(e: CoordinateError) -> Self {
        Self::Coordinate(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// AppendReceipt: proof an event was persisted.
#[derive(Clone, Debug)]
pub struct AppendReceipt {
    pub event_id: u128,
    pub sequence: u64,
    pub disk_pos: DiskPos,
}

/// AppendOptions: CAS, idempotency, custom correlation/causation.
/// [SPEC:src/store/mod.rs — AppendOptions]
#[derive(Clone, Copy, Debug, Default)]
pub struct AppendOptions {
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
    pub correlation_id: Option<u128>,
    pub causation_id: Option<u128>,
    /// EventHeader flags (FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    /// Default: 0 (no flags). [SPEC:src/event/header.rs — Flag bit constants]
    pub flags: u8,
}

impl AppendOptions {
    /// Create new AppendOptions with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set expected sequence for compare-and-swap (CAS) check.
    pub fn with_cas(mut self, seq: u32) -> Self {
        self.expected_sequence = Some(seq);
        self
    }

    /// Set idempotency key. Duplicate appends with the same key return the original receipt.
    pub fn with_idempotency(mut self, key: u128) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// Set EventHeader flags (bitwise OR of FLAG_REQUIRES_ACK, FLAG_TRANSACTIONAL, FLAG_REPLAY).
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// Set custom correlation ID.
    pub fn with_correlation(mut self, id: u128) -> Self {
        self.correlation_id = Some(id);
        self
    }

    /// Set custom causation ID.
    pub fn with_causation(mut self, id: u128) -> Self {
        self.causation_id = Some(id);
        self
    }
}

/// Predicate for filtering events during compaction. Returns true to keep, false to drop.
pub type RetentionPredicate = Box<dyn Fn(&StoredEvent<serde_json::Value>) -> bool + Send>;

/// CompactionStrategy: how compact() handles events during segment merging.
#[non_exhaustive]
pub enum CompactionStrategy {
    /// Merge sealed segments into one. No events removed.
    Merge,
    /// Merge + drop events failing the retention predicate.
    /// Dropped events are permanently lost.
    Retention(RetentionPredicate),
    /// Merge + write tombstone markers for dropped events.
    /// Downstream consumers can detect deletions.
    Tombstone(RetentionPredicate),
}

/// CompactionConfig: controls compact() behavior.
pub struct CompactionConfig {
    /// Strategy for handling events during compaction.
    pub strategy: CompactionStrategy,
    /// Minimum number of sealed segments before compaction runs.
    /// Below this threshold, compact() returns early.
    pub min_segments: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            strategy: CompactionStrategy::Merge,
            min_segments: 2,
        }
    }
}

impl Store {
    /// Open a store with default config at `./batpak-data`.
    /// Sugar over `Store::open(StoreConfig::new("./batpak-data"))`.
    /// [SPEC:src/store/mod.rs — Store::open_default]
    pub fn open_default() -> Result<Self, StoreError> {
        Self::open(StoreConfig::new("./batpak-data"))
    }

    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_cache(config, Box::new(NoCache))
    }

    pub fn open_with_cache(
        config: StoreConfig,
        cache: Box<dyn ProjectionCache>,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let config = Arc::new(config);
        let index = Arc::new(StoreIndex::new());
        let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

        // Cold start: scan all segments, rebuild index.
        // [SPEC:IMPLEMENTATION NOTES item 2 — segment naming, alphabetical scan]
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&config.data_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == segment::SEGMENT_EXTENSION)
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for dir_entry in &entries {
            let scanned = reader.scan_segment(&dir_entry.path())?;
            for se in scanned {
                let coord = Coordinate::new(&se.entity, &se.scope)?;
                let clock = se.event.header.position.sequence;
                let entry = IndexEntry {
                    event_id: se.event.header.event_id,
                    correlation_id: se.event.header.correlation_id,
                    causation_id: se.event.header.causation_id,
                    coord,
                    kind: se.event.header.event_kind,
                    wall_ms: se.event.header.position.wall_ms,
                    clock,
                    hash_chain: se.event.hash_chain.clone().unwrap_or_default(),
                    disk_pos: DiskPos {
                        segment_id: se.segment_id,
                        offset: se.offset,
                        length: se.length,
                    },
                    global_sequence: index.global_sequence(),
                };
                index.insert(entry);
            }
        }

        let subscribers = Arc::new(SubscriberList::new());
        let writer = WriterHandle::spawn(&config, &index, &subscribers)?;

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
    pub fn append(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id,
            event_id,
            None, // correlation = self, causation = root
            self.config.now_us(),
            crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32,
            kind,
        );
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
                    correlation_id: event_id,
                    causation_id: None,
                    expected_sequence: None,
                    idempotency_key: None,
                },
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: append a reaction (caused by another event).
    pub fn append_reaction(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        correlation_id: u128,
        causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id,
            correlation_id,
            Some(causation_id),
            self.config.now_us(),
            crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32,
            kind,
        );
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
                    causation_id: Some(causation_id),
                    expected_sequence: None,
                    idempotency_key: None,
                },
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// READ: get a single event by ID.
    pub fn get(&self, event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let entry = self
            .index
            .get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: query by Region.
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
        let mut results = Vec::new();
        #[cfg(feature = "blake3")]
        {
            let mut current_id = Some(event_id);
            while let Some(id) = current_id {
                if results.len() >= limit {
                    break;
                }
                if let Some(entry) = self.index.get_by_id(id) {
                    if let Ok(stored) = self.reader.read_entry(&entry.disk_pos) {
                        results.push(stored);
                    }
                    // Follow prev_hash: find the entry whose event_hash matches prev_hash
                    let prev = entry.hash_chain.prev_hash;
                    if prev == [0u8; 32] {
                        break;
                    } // genesis
                      // Linear scan is acceptable for ancestor walks (bounded by limit).
                    current_id = self
                        .index
                        .stream(entry.coord.entity())
                        .iter()
                        .find(|e| e.hash_chain.event_hash == prev)
                        .map(|e| e.event_id);
                } else {
                    break;
                }
            }
        }

        #[cfg(not(feature = "blake3"))]
        {
            // Without blake3, hashes are all zeros. Walk by descending clock order.
            let Some(start_entry) = self.index.get_by_id(event_id) else {
                return results;
            };
            let entity = start_entry.coord.entity();
            let stream = self.index.stream(entity);
            // stream is sorted by (clock, uuid). Walk backwards from start_entry's clock.
            for entry in stream.iter().rev() {
                if results.len() >= limit {
                    break;
                }
                if entry.clock > start_entry.clock {
                    continue;
                }
                if let Ok(stored) = self.reader.read_entry(&entry.disk_pos) {
                    results.push(stored);
                }
            }
        }

        results
    }

    /// PROJECT: reconstruct typed state from events, with cache support.
    /// [SPEC:src/store/mod.rs — Projection Flow]
    pub fn project<T>(&self, entity: &str, freshness: &Freshness) -> Result<Option<T>, StoreError>
    where
        T: EventSourced<serde_json::Value> + serde::Serialize + serde::de::DeserializeOwned,
    {
        let entries = self.index.stream(entity);
        if entries.is_empty() {
            return Ok(None);
        }

        let watermark = entries.last().map(|e| e.global_sequence).unwrap_or(0);
        let cache_key = entity.as_bytes();

        // Step 0: Prefetch hint — let cache pre-warm if it supports it.
        // [SPEC:src/store/projection.rs — ProjectionCache::prefetch]
        let predicted_meta = projection::CacheMeta {
            watermark,
            cached_at_us: self.config.now_us(),
        };
        if let Err(e) = self.cache.prefetch(cache_key, predicted_meta) {
            tracing::warn!("cache prefetch failed (non-fatal): {e}");
        }

        // Step 1: Check cache
        if let Ok(Some((bytes, meta))) = self.cache.get(cache_key) {
            let is_fresh = match freshness {
                Freshness::Consistent => meta.watermark == watermark,
                Freshness::BestEffort { max_stale_ms } => {
                    let age_us = self
                        .config
                        .now_us()
                        .saturating_sub(meta.cached_at_us)
                        .max(0);
                    age_us < (*max_stale_ms as i64) * 1000
                }
            };
            if is_fresh {
                if let Ok(t) = serde_json::from_slice::<T>(&bytes) {
                    return Ok(Some(t));
                }
                // Deserialization failed — fall through to replay
            }
        }

        // Step 2: Cache miss or stale — replay from segments
        let mut events = Vec::with_capacity(entries.len());
        for entry in &entries {
            let stored = self.reader.read_entry(&entry.disk_pos)?;
            events.push(stored.event);
        }
        let result = T::from_events(&events);

        // Step 3: Populate cache (non-fatal on error)
        if let Some(ref t) = result {
            if let Ok(bytes) = serde_json::to_vec(t) {
                let meta = projection::CacheMeta {
                    watermark,
                    cached_at_us: self.config.now_us(),
                };
                if let Err(e) = self.cache.put(cache_key, &bytes, meta) {
                    tracing::warn!("cache put failed (non-fatal): {e}");
                }
            }
        }

        Ok(result)
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

    /// CONVENIENCE: sugar over Region.
    pub fn stream(&self, entity: &str) -> Vec<IndexEntry> {
        self.query(&Region::entity(entity))
    }
    pub fn by_scope(&self, scope: &str) -> Vec<IndexEntry> {
        self.query(&Region::scope(scope))
    }
    pub fn by_fact(&self, kind: EventKind) -> Vec<IndexEntry> {
        self.query(&Region::all().with_fact(KindFilter::Exact(kind)))
    }

    /// REACT: spawn a background thread running the subscribe→react→append loop.
    /// Returns a JoinHandle. The thread runs until the store is dropped (subscription closes).
    /// [SPEC:src/event/sourcing.rs — Reactive<P> glue pattern]
    pub fn react_loop<R>(
        self: &Arc<Self>,
        region: &Region,
        reactor: R,
    ) -> std::thread::JoinHandle<()>
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
            .expect("failed to spawn reactor thread")
    }

    /// WRITE: append with CAS, idempotency, custom correlation/causation.
    /// CAS and idempotency checks execute inside the writer thread under
    /// the entity lock — no TOCTOU race between check and commit.
    pub fn append_with_options(
        &self,
        coord: &Coordinate,
        kind: EventKind,
        payload: &impl Serialize,
        opts: AppendOptions,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = opts
            .idempotency_key
            .unwrap_or_else(crate::id::generate_v7_id);
        let correlation_id = opts.correlation_id.unwrap_or(event_id);
        let causation_id = opts.causation_id;
        let header = EventHeader::new(
            event_id,
            correlation_id,
            causation_id,
            self.config.now_us(),
            crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32,
            kind,
        )
        .with_flags(opts.flags);
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
                    expected_sequence: opts.expected_sequence,
                    idempotency_key: opts.idempotency_key,
                },
                respond: tx,
            })
            .map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: apply a typestate transition — extracts kind+payload, delegates to append.
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
    pub fn sync(&self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer
            .tx
            .send(WriterCommand::Sync { respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// Snapshot the current index to a destination directory.
    pub fn snapshot(&self, dest: &std::path::Path) -> Result<(), StoreError> {
        self.sync()?;
        // Copy all segment files to dest
        std::fs::create_dir_all(dest).map_err(StoreError::Io)?;
        let entries = std::fs::read_dir(&self.config.data_dir).map_err(StoreError::Io)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .map(|e| e == segment::SEGMENT_EXTENSION)
                .unwrap_or(false)
            {
                let dest_path = dest.join(entry.file_name());
                std::fs::copy(&path, &dest_path).map_err(StoreError::Io)?;
            }
        }
        Ok(())
    }

    /// Compact: merge sealed segments, optionally filtering events.
    /// Returns the number of segments removed and bytes reclaimed.
    /// The active (currently-written) segment is never touched.
    pub fn compact(
        &self,
        config: &CompactionConfig,
    ) -> Result<segment::CompactionResult, StoreError> {
        self.sync()?;

        // 1. Enumerate sealed segment files (not the active one the writer owns).
        // The active segment is always the max-numbered one.
        let active_segment_id = std::fs::read_dir(&self.config.data_dir)
            .map_err(StoreError::Io)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path
                    .extension()
                    .map(|ext| ext == segment::SEGMENT_EXTENSION)
                    .unwrap_or(false)
                {
                    path.file_stem()?.to_str()?.parse::<u64>().ok()
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);

        let mut sealed: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&self.config.data_dir)
            .map_err(StoreError::Io)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                let ext_ok = path
                    .extension()
                    .map(|ext| ext == segment::SEGMENT_EXTENSION)
                    .unwrap_or(false);
                if !ext_ok {
                    return None;
                }
                let seg_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.parse::<u64>().ok())?;
                if seg_id >= active_segment_id {
                    return None;
                } // skip active
                Some((seg_id, path))
            })
            .collect();
        sealed.sort_by_key(|(id, _)| *id);

        if sealed.len() < config.min_segments {
            return Ok(segment::CompactionResult {
                segments_removed: 0,
                bytes_reclaimed: 0,
            });
        }

        // 2. Read all events from sealed segments
        let mut all_events: Vec<reader::ScannedEntry> = Vec::new();
        for (_, path) in &sealed {
            let scanned = self.reader.scan_segment(path)?;
            all_events.extend(scanned);
        }

        // 3. Apply strategy filter
        let tombstone_kind = EventKind::custom(0x0, 0xFFE); // system tombstone
        let mut kept_events: Vec<reader::ScannedEntry> = Vec::new();
        match &config.strategy {
            CompactionStrategy::Merge => {
                kept_events = all_events;
            }
            CompactionStrategy::Retention(predicate) => {
                for entry in all_events {
                    let coord = Coordinate::new(&entry.entity, &entry.scope)?;
                    let stored = StoredEvent {
                        coordinate: coord,
                        event: entry.event.clone(),
                    };
                    if predicate(&stored) {
                        kept_events.push(entry);
                    }
                }
            }
            CompactionStrategy::Tombstone(predicate) => {
                for entry in all_events {
                    let coord = Coordinate::new(&entry.entity, &entry.scope)?;
                    let stored = StoredEvent {
                        coordinate: coord,
                        event: entry.event.clone(),
                    };
                    if predicate(&stored) {
                        kept_events.push(entry);
                    } else {
                        let mut tombstone = entry;
                        tombstone.event.header.event_kind = tombstone_kind;
                        kept_events.push(tombstone);
                    }
                }
            }
        }

        // 4. Create merged segment.
        // Use the lowest sealed ID for the merged segment (reuse it).
        let merged_id = sealed[0].0;
        let merged_path = self
            .config
            .data_dir
            .join(segment::segment_filename(merged_id));

        // Evict FDs for ALL sealed segments before removing files
        for (seg_id, _) in &sealed {
            self.reader.evict_segment(*seg_id);
        }

        let _ = std::fs::remove_file(&merged_path); // remove the existing file at merged_id
        let mut merged_segment =
            segment::Segment::<segment::Active>::create(&self.config.data_dir, merged_id)?;

        // 5. Write kept events to merged segment
        for entry in &kept_events {
            let frame_payload = segment::FramePayload {
                event: entry.event.clone(),
                entity: entry.entity.clone(),
                scope: entry.scope.clone(),
            };
            let frame = segment::frame_encode(&frame_payload)?;
            merged_segment.write_frame(&frame)?;
        }

        merged_segment.sync()?;
        let _sealed_seg = merged_segment.seal();

        // 6. Delete old segment files (except the merged one which was replaced)
        let mut bytes_reclaimed: u64 = 0;
        let mut segments_removed: usize = 0;
        for (seg_id, path) in &sealed {
            if *seg_id == merged_id {
                continue;
            } // already replaced
            if let Ok(meta) = std::fs::metadata(path) {
                bytes_reclaimed += meta.len();
            }
            std::fs::remove_file(path).map_err(StoreError::Io)?;
            segments_removed += 1;
        }

        // 7. Rebuild index from all remaining segments on disk.
        // This guarantees consistency for Retention (dropped events removed)
        // and Tombstone (event_kind updated in index).
        self.index.clear();
        let mut remaining: Vec<std::fs::DirEntry> = std::fs::read_dir(&self.config.data_dir)
            .map_err(StoreError::Io)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == segment::SEGMENT_EXTENSION)
                    .unwrap_or(false)
            })
            .collect();
        remaining.sort_by_key(|e| e.file_name());

        for dir_entry in &remaining {
            let scanned = self.reader.scan_segment(&dir_entry.path())?;
            for se in scanned {
                let coord = Coordinate::new(&se.entity, &se.scope)?;
                let clock = se.event.header.position.sequence;
                let entry = IndexEntry {
                    event_id: se.event.header.event_id,
                    correlation_id: se.event.header.correlation_id,
                    causation_id: se.event.header.causation_id,
                    coord,
                    kind: se.event.header.event_kind,
                    wall_ms: se.event.header.position.wall_ms,
                    clock,
                    hash_chain: se.event.hash_chain.clone().unwrap_or_default(),
                    disk_pos: DiskPos {
                        segment_id: se.segment_id,
                        offset: se.offset,
                        length: se.length,
                    },
                    global_sequence: self.index.global_sequence(),
                };
                self.index.insert(entry);
            }
        }

        Ok(segment::CompactionResult {
            segments_removed,
            bytes_reclaimed,
        })
    }

    pub fn close(self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer
            .tx
            .send(WriterCommand::Shutdown { respond: tx })
            .map_err(|_| StoreError::WriterCrashed)?;
        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// DIAGNOSTICS
    pub fn stats(&self) -> StoreStats {
        StoreStats {
            event_count: self.index.len(),
            global_sequence: self.index.global_sequence(),
        }
    }

    pub fn diagnostics(&self) -> StoreDiagnostics {
        StoreDiagnostics {
            event_count: self.index.len(),
            global_sequence: self.index.global_sequence(),
            data_dir: self.config.data_dir.clone(),
            segment_max_bytes: self.config.segment_max_bytes,
            fd_budget: self.config.fd_budget,
            restart_policy: self.config.restart_policy.clone(),
        }
    }
}

/// Safety net: if Store is dropped without calling close(), send a best-effort
/// Shutdown to the writer thread. close(self) is still the preferred explicit path.
impl Drop for Store {
    fn drop(&mut self) {
        let (tx, _rx) = flume::bounded(1);
        let _ = self.writer.tx.send(WriterCommand::Shutdown { respond: tx });
    }
}

pub(crate) fn now_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}

#[derive(Clone, Debug)]
pub struct StoreStats {
    pub event_count: usize,
    pub global_sequence: u64,
}

#[derive(Clone, Debug)]
pub struct StoreDiagnostics {
    pub event_count: usize,
    pub global_sequence: u64,
    pub data_dir: PathBuf,
    pub segment_max_bytes: u64,
    pub fd_budget: usize,
    pub restart_policy: RestartPolicy,
}
