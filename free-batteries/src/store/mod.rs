pub mod index;
pub mod segment;
pub mod writer;
pub mod reader;
pub mod projection;
pub mod cursor;
pub mod subscription;

pub use index::{IndexEntry, ClockKey, DiskPos};
pub use projection::{ProjectionCache, NoCache, CacheMeta, Freshness};
pub use cursor::Cursor;
pub use subscription::Subscription;
pub use writer::{Notification, RestartPolicy};

use crate::coordinate::{Coordinate, CoordinateError, Region, KindFilter};
use crate::event::{Event, EventHeader, EventKind, StoredEvent, EventSourced};
use index::StoreIndex;
use reader::Reader;
use writer::{WriterHandle, WriterCommand, AppendGuards, SubscriberList};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
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
    _cache: Box<dyn ProjectionCache>,
    writer: WriterHandle,
    config: Arc<StoreConfig>,
}

/// StoreConfig: all settings with sane defaults.
#[derive(Clone, Debug)]
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
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./free-batteries-data"),
            segment_max_bytes: 256 * 1024 * 1024,  // 256MB
            sync_every_n_events: 1000,
            fd_budget: 64,
            writer_channel_capacity: 4096,
            broadcast_capacity: 8192,
            cache_map_size_bytes: 64 * 1024 * 1024, // 64MB
            restart_policy: RestartPolicy::default(),
            shutdown_drain_limit: 1024,
        }
    }
}

/// StoreError: every error the store can produce.
/// [SPEC:src/store/mod.rs — StoreError variants]
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Coordinate(CoordinateError),
    Serialization(String),
    CrcMismatch { segment_id: u64, offset: u64 },
    CorruptSegment { segment_id: u64, detail: String },
    NotFound(u128),
    SequenceMismatch { entity: String, expected: u32, actual: u32 },
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
            Self::CrcMismatch { segment_id, offset } =>
                write!(f, "CRC mismatch in segment {segment_id} at offset {offset}"),
            Self::CorruptSegment { segment_id, detail } =>
                write!(f, "corrupt segment {segment_id}: {detail}"),
            Self::NotFound(id) => write!(f, "event {id:032x} not found"),
            Self::SequenceMismatch { entity, expected, actual } =>
                write!(f, "CAS failed for {entity}: expected seq {expected}, got {actual}"),
            Self::DuplicateEvent(key) => write!(f, "duplicate idempotency key {key:032x}"),
            Self::WriterCrashed => write!(f, "writer thread crashed"),
            Self::ShuttingDown => write!(f, "store is shutting down"),
            Self::CacheFailed(s) => write!(f, "cache error: {s}"),
        }
    }
}
impl std::error::Error for StoreError {}
impl From<CoordinateError> for StoreError {
    fn from(e: CoordinateError) -> Self { Self::Coordinate(e) }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
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
}

impl Store {
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let config = Arc::new(config);
        let index = Arc::new(StoreIndex::new());
        let reader = Arc::new(Reader::new(config.data_dir.clone(), config.fd_budget));

        // Cold start: scan all segments, rebuild index.
        // [SPEC:IMPLEMENTATION NOTES item 2 — segment naming, alphabetical scan]
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&config.data_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "fbat").unwrap_or(false))
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
            index, reader, _cache: Box::new(NoCache), writer, config,
        })
    }

    pub fn open_default() -> Result<Self, StoreError> {
        Self::open(StoreConfig::default())
    }

    /// WRITE: append a new root-cause event.
    /// correlation_id defaults to event_id (self-correlated). causation_id = None.
    pub fn append(
        &self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id, event_id, None, // correlation = self, causation = root
            now_us(), crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32, kind,
        );
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Append {
            entity: coord.entity_arc(),
            scope: coord.scope_arc(),
            event: Box::new(event), kind,
            guards: AppendGuards {
                correlation_id: event_id, causation_id: None,
                expected_sequence: None, idempotency_key: None,
            },
            respond: tx,
        }).map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// WRITE: append a reaction (caused by another event).
    pub fn append_reaction(
        &self, coord: &Coordinate, kind: EventKind, payload: &impl Serialize,
        correlation_id: u128, causation_id: u128,
    ) -> Result<AppendReceipt, StoreError> {
        let payload_bytes = rmp_serde::to_vec_named(payload)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        let event_id = crate::id::generate_v7_id();
        let header = EventHeader::new(
            event_id, correlation_id, Some(causation_id),
            now_us(), crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32, kind,
        );
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Append {
            entity: coord.entity_arc(), scope: coord.scope_arc(),
            event: Box::new(event), kind,
            guards: AppendGuards {
                correlation_id, causation_id: Some(causation_id),
                expected_sequence: None, idempotency_key: None,
            },
            respond: tx,
        }).map_err(|_| StoreError::WriterCrashed)?;

        rx.recv().map_err(|_| StoreError::WriterCrashed)?
    }

    /// READ: get a single event by ID.
    pub fn get(&self, event_id: u128) -> Result<StoredEvent<serde_json::Value>, StoreError> {
        let entry = self.index.get_by_id(event_id)
            .ok_or(StoreError::NotFound(event_id))?;
        self.reader.read_entry(&entry.disk_pos)
    }

    /// READ: query by Region.
    pub fn query(&self, region: &Region) -> Vec<IndexEntry> {
        self.index.query(region)
    }

    /// READ: walk hash chain ancestors. [SPEC:IMPLEMENTATION NOTES item 3]
    /// When blake3 is enabled, follows the hash chain (event_hash → prev_hash).
    /// When blake3 is disabled, all hashes are [0u8;32] so hash-based walking
    /// is impossible. Falls back to clock-ordered traversal (descending clock).
    pub fn walk_ancestors(&self, event_id: u128, limit: usize)
        -> Vec<StoredEvent<serde_json::Value>>
    {
        let mut results = Vec::new();
        #[cfg(feature = "blake3")]
        {
            let mut current_id = Some(event_id);
            while let Some(id) = current_id {
                if results.len() >= limit { break; }
                if let Some(entry) = self.index.get_by_id(id) {
                    if let Ok(stored) = self.reader.read_entry(&entry.disk_pos) {
                        results.push(stored);
                    }
                    // Follow prev_hash: find the entry whose event_hash matches prev_hash
                    let prev = entry.hash_chain.prev_hash;
                    if prev == [0u8; 32] { break; } // genesis
                    // Linear scan is acceptable for ancestor walks (bounded by limit).
                    current_id = self.index.stream(entry.coord.entity())
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
                if results.len() >= limit { break; }
                if entry.clock > start_entry.clock { continue; }
                if let Ok(stored) = self.reader.read_entry(&entry.disk_pos) {
                    results.push(stored);
                }
            }
        }

        results
    }

    /// PROJECT: reconstruct typed state from events.
    pub fn project<T: EventSourced<serde_json::Value>>(
        &self, entity: &str, _freshness: Freshness,
    ) -> Result<Option<T>, StoreError> {
        let entries = self.index.stream(entity);
        if entries.is_empty() { return Ok(None); }

        let mut events = Vec::with_capacity(entries.len());
        for entry in &entries {
            let stored = self.reader.read_entry(&entry.disk_pos)?;
            events.push(stored.event);
        }
        Ok(T::from_events(&events))
    }

    /// SUBSCRIBE: push-based, lossy.
    pub fn subscribe(&self, region: &Region) -> Subscription {
        let rx = self.writer.subscribers.subscribe(self.config.broadcast_capacity);
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
        let event_id = opts.idempotency_key.unwrap_or_else(crate::id::generate_v7_id);
        let correlation_id = opts.correlation_id.unwrap_or(event_id);
        let causation_id = opts.causation_id;
        let header = EventHeader::new(
            event_id, correlation_id, causation_id,
            now_us(), crate::coordinate::DagPosition::root(),
            payload_bytes.len() as u32, kind,
        );
        let event = Event::new(header, payload_bytes);

        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Append {
            entity: coord.entity_arc(),
            scope: coord.scope_arc(),
            event: Box::new(event), kind,
            guards: AppendGuards {
                correlation_id, causation_id,
                expected_sequence: opts.expected_sequence,
                idempotency_key: opts.idempotency_key,
            },
            respond: tx,
        }).map_err(|_| StoreError::WriterCrashed)?;

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
        self.writer.tx.send(WriterCommand::Sync { respond: tx })
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
            if path.extension().map(|e| e == "fbat").unwrap_or(false) {
                let dest_path = dest.join(entry.file_name());
                std::fs::copy(&path, &dest_path).map_err(StoreError::Io)?;
            }
        }
        Ok(())
    }

    /// Compact: placeholder for segment compaction. Currently a no-op sync.
    pub fn compact(&self) -> Result<(), StoreError> {
        // Future: merge segments, remove superseded events.
        // For now, just sync to ensure durability.
        self.sync()
    }

    pub fn close(self) -> Result<(), StoreError> {
        let (tx, rx) = flume::bounded(1);
        self.writer.tx.send(WriterCommand::Shutdown { respond: tx })
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

fn now_us() -> i64 {
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
