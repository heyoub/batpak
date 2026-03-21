#[allow(unexpected_cfgs)]
#[cfg(feature = "exponential-backoff")]
compile_error!(
    "Red flag: only Once and Bounded restart policies. \
     Exponential backoff belongs in the product's supervisor, not here. \
     See: SPEC.md ## RED FLAGS."
);

use crate::coordinate::{Coordinate, DagPosition};
use crate::event::{Event, EventKind, HashChain};
use crate::store::index::{DiskPos, IndexEntry, StoreIndex};
use crate::store::segment::{self, Active, FramePayload, Segment};
use crate::store::{AppendReceipt, StoreConfig, StoreError};
use flume::{Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::{debug, info, trace};

/// WriterCommand: messages sent to the background writer thread via flume.
/// All respond channels: flume::Sender — sync send from writer, async recv from caller.
/// [SPEC:src/store/writer.rs]
pub(crate) enum WriterCommand {
    Append {
        entity: Arc<str>,
        scope: Arc<str>,
        event: Box<Event<Vec<u8>>>, // pre-serialized payload as msgpack bytes
        kind: EventKind,
        guards: AppendGuards,
        respond: Sender<Result<AppendReceipt, StoreError>>,
    },
    Sync {
        respond: Sender<Result<(), StoreError>>,
    },
    Shutdown {
        respond: Sender<Result<(), StoreError>>,
    },
}

/// WriterHandle: owned by Store. Communicates with the background thread.
pub(crate) struct WriterHandle {
    pub tx: Sender<WriterCommand>,
    pub subscribers: Arc<SubscriberList>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

/// SubscriberList: push-based notification fanout via flume channels.
/// [SPEC:src/store/writer.rs — try_send pattern]
pub(crate) struct SubscriberList {
    senders: Mutex<Vec<Sender<Notification>>>,
}

/// Notification: lightweight event summary pushed to subscribers.
/// Must derive Clone (used in try_send broadcast loop).
/// [SPEC:src/store/writer.rs — Notification struct]
#[derive(Clone, Debug)]
pub struct Notification {
    pub event_id: u128,
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub coord: Coordinate,
    pub kind: EventKind,
    pub sequence: u64,
}

/// RestartPolicy: how the writer recovers from panics.
/// [SPEC:src/store/writer.rs — RestartPolicy]
/// EXACTLY two variants. Adding a third violates the RED FLAGS.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub enum RestartPolicy {
    #[default]
    Once,
    Bounded {
        max_restarts: u32,
        within_ms: u64,
    },
}

impl SubscriberList {
    pub(crate) fn new() -> Self {
        Self {
            senders: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe: create a new bounded channel, store the sender, return the receiver.
    pub(crate) fn subscribe(&self, capacity: usize) -> Receiver<Notification> {
        let (tx, rx) = flume::bounded(capacity);
        self.senders.lock().push(tx);
        rx
    }

    /// Broadcast: try_send to all, retain on Ok or Full, prune on Disconnected.
    /// NEVER use blocking send() — one slow subscriber must not block the writer.
    /// [DEP:flume::Sender::try_send] → Result<(), TrySendError<T>>
    /// [DEP:flume::TrySendError::Full] / [DEP:flume::TrySendError::Disconnected]
    pub(crate) fn broadcast(&self, notif: &Notification) {
        let mut guard = self.senders.lock();
        guard.retain(|tx| match tx.try_send(notif.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

impl WriterHandle {
    /// Spawn the background writer thread.
    /// [SPEC:src/store/writer.rs — "free-batteries-writer" thread]
    pub(crate) fn spawn(
        config: &Arc<StoreConfig>,
        index: &Arc<StoreIndex>,
        subscribers: &Arc<SubscriberList>,
    ) -> Result<Self, StoreError> {
        // Fallible init — propagate errors to Store::open() caller
        std::fs::create_dir_all(&config.data_dir).map_err(StoreError::Io)?;
        let initial_segment_id = find_latest_segment_id(&config.data_dir).unwrap_or(0) + 1;
        let initial_segment = Segment::<Active>::create(&config.data_dir, initial_segment_id)?;

        let (tx, rx) = flume::bounded::<WriterCommand>(config.writer_channel_capacity);
        let subs = Arc::clone(subscribers);
        let cfg = Arc::clone(config);
        let idx = Arc::clone(index);

        let thread_name = format!("free-batteries-writer-{:08x}", {
            let mut h: u64 = 0xcbf29ce484222325; // FNV-1a basis
            for b in config.data_dir.to_string_lossy().bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
            }
            h
        });

        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                writer_loop(&rx, &cfg, &idx, &subs, initial_segment, initial_segment_id);
            })
            .map_err(StoreError::Io)?;

        Ok(Self {
            tx,
            subscribers: Arc::clone(subscribers),
            _thread: Some(thread),
        })
    }

    // NOTE: No send_append() method here. Store::append() and Store::append_reaction()
    // in store/mod.rs create one-shot flume channels and send WriterCommands directly
    // via self.writer.tx.send(). This avoids an unnecessary abstraction layer.
    // WriterHandle.tx is pub(crate) for direct access. [SPEC:INVARIANTS item 4]
}

/// Writer's mutable runtime state, grouped to reduce handle_append param count.
struct WriterState<'a> {
    index: &'a StoreIndex,
    active_segment: &'a mut Segment<Active>,
    segment_id: &'a mut u64,
    config: &'a StoreConfig,
    subscribers: &'a SubscriberList,
}

/// The writer's main loop. Runs on the background thread.
/// The spawn closure owns the Arcs; this function borrows them.
fn writer_loop(
    rx: &Receiver<WriterCommand>,
    config: &StoreConfig,
    index: &StoreIndex,
    subscribers: &SubscriberList,
    mut active_segment: Segment<Active>,
    mut segment_id: u64,
) {
    let mut events_since_sync: u32 = 0;

    let mut state = WriterState {
        index,
        active_segment: &mut active_segment,
        segment_id: &mut segment_id,
        config,
        subscribers,
    };

    // Main loop: recv commands, dispatch.
    for cmd in rx.iter() {
        match cmd {
            WriterCommand::Append {
                entity,
                scope,
                event,
                kind,
                guards,
                respond,
            } => {
                let result = state.handle_append(&entity, &scope, *event, kind, &guards);
                // Respond to caller. Ignore send error (caller may have dropped).
                let _ = respond.send(result);

                events_since_sync += 1;
                if events_since_sync >= config.sync_every_n_events {
                    let _ = state.active_segment.sync();
                    events_since_sync = 0;
                }
            }
            WriterCommand::Sync { respond } => {
                let result = state.active_segment.sync();
                let _ = respond.send(result);
                events_since_sync = 0;
            }
            WriterCommand::Shutdown { respond } => {
                // Drain up to shutdown_drain_limit queued commands.
                // [SPEC:src/store/writer.rs — Shutdown drain semantics]
                let mut drained = 0;
                while drained < config.shutdown_drain_limit {
                    match rx.try_recv() {
                        Ok(WriterCommand::Append {
                            entity,
                            scope,
                            event,
                            kind,
                            guards,
                            respond: r,
                        }) => {
                            let result =
                                state.handle_append(&entity, &scope, *event, kind, &guards);
                            let _ = r.send(result);
                            drained += 1;
                        }
                        Ok(WriterCommand::Shutdown { respond: r }) => {
                            let _ = r.send(Ok(()));
                        }
                        Ok(WriterCommand::Sync { respond: r }) => {
                            let _ = r.send(state.active_segment.sync());
                        }
                        Err(_) => break, // channel empty
                    }
                }
                let _ = state.active_segment.sync();
                let _ = respond.send(Ok(()));
                return; // exit writer loop
            }
        }
    }
}

/// Options and guards for an append operation, passed through the channel.
/// CAS + idempotency checks execute under the entity lock — no TOCTOU.
pub(crate) struct AppendGuards {
    pub correlation_id: u128,
    pub causation_id: Option<u128>,
    pub expected_sequence: Option<u32>,
    pub idempotency_key: Option<u128>,
}

impl WriterState<'_> {
    /// The 10-step commit protocol.
    /// [SPEC:src/store/writer.rs — handle_append]
    fn handle_append(
        &mut self,
        entity: &Arc<str>,
        scope: &Arc<str>,
        mut event: Event<Vec<u8>>,
        kind: EventKind,
        guards: &AppendGuards,
    ) -> Result<AppendReceipt, StoreError> {
        let correlation_id = guards.correlation_id;
        let causation_id = guards.causation_id;

        // STEP 1: Acquire per-entity lock.
        // [SPEC:IMPLEMENTATION NOTES item 5 — DashMap guard lifetimes]
        // Clone the Arc<Mutex> OUT of DashMap, drop the DashMap entry guard,
        // THEN lock the Mutex. Never hold DashMap Ref across the commit.
        let lock = self
            .index
            .entity_locks
            .entry(Arc::clone(entity))
            .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
            .clone();
        let _entity_guard = lock.lock();
        trace!(entity = %entity, "entity lock acquired");

        // STEP 1a: CAS check (under entity lock — no TOCTOU).
        if let Some(expected) = guards.expected_sequence {
            let actual = self.index.get_latest(entity).map(|e| e.clock).unwrap_or(0);
            if actual != expected {
                return Err(StoreError::SequenceMismatch {
                    entity: entity.to_string(),
                    expected,
                    actual,
                });
            }
        }

        // STEP 1b: Idempotency check (under entity lock — no TOCTOU).
        if let Some(key) = guards.idempotency_key {
            if let Some(entry) = self.index.get_by_id(key) {
                return Ok(AppendReceipt {
                    event_id: entry.event_id,
                    sequence: entry.global_sequence,
                    disk_pos: entry.disk_pos,
                });
            }
        }

        // STEP 2: Get prev_hash from index (or [0u8;32] for genesis).
        // Clone the value out of the DashMap Ref immediately.
        let prev_hash = self
            .index
            .get_latest(entity)
            .map(|e| e.hash_chain.event_hash)
            .unwrap_or([0u8; 32]);

        // STEP 3: Compute sequence (latest.clock + 1, or 0).
        let clock = self
            .index
            .get_latest(entity)
            .map(|e| e.clock + 1)
            .unwrap_or(0);

        // STEP 4: Set event header position with HLC wall clock.
        // [CROSS-POLLINATION:czap/hlc.ts — HLC for global causal ordering]
        let now_ms = (event.header.timestamp_us / 1000) as u64;
        let position = DagPosition::child_at(clock, now_ms, 0);
        event.header.position = position;
        event.header.event_kind = kind;
        event.header.correlation_id = correlation_id;
        event.header.causation_id = causation_id;

        // STEP 5: Compute blake3 hash, set hash chain (or skip if feature off).
        // [SPEC:INVARIANTS item 5 — blake3 only]
        #[cfg(feature = "blake3")]
        let event_hash = crate::event::hash::compute_hash(&event.payload);
        #[cfg(not(feature = "blake3"))]
        let event_hash = [0u8; 32];

        event.hash_chain = Some(HashChain {
            prev_hash,
            event_hash,
        });
        // Set content_hash on header for projection cache auto-invalidation.
        // [CROSS-POLLINATION:czap/typed-ref.ts — content addressing]
        event.header.content_hash = event_hash;

        // STEP 6: Serialize to MessagePack + CRC32 frame.
        // [SPEC:WIRE FORMAT DECISIONS — rmp_serde::to_vec_named() ALWAYS]
        let frame_payload = FramePayload {
            event: event.clone(),
            entity: entity.to_string(),
            scope: scope.to_string(),
        };
        let frame = segment::frame_encode(&frame_payload)?;

        // STEP 7: Check segment rotation.
        if self
            .active_segment
            .needs_rotation(self.config.segment_max_bytes)
        {
            self.active_segment.sync()?;
            let old = std::mem::replace(
                self.active_segment,
                Segment::<Active>::create(&self.config.data_dir, *self.segment_id + 1)?,
            );
            let _sealed = old.seal();
            *self.segment_id += 1;
            info!(segment_id = *self.segment_id, "segment rotated");
        }

        // STEP 8: Write frame to segment file.
        let offset = self.active_segment.write_frame(&frame)?;
        trace!(offset = offset, len = frame.len(), "frame written");

        // STEP 9: Update index.
        let global_seq = self.index.global_sequence();
        let disk_pos = DiskPos {
            segment_id: *self.segment_id,
            offset,
            length: frame.len() as u32,
        };
        let entry = IndexEntry {
            event_id: event.header.event_id,
            correlation_id,
            causation_id,
            coord: Coordinate::new(entity.as_ref(), scope.as_ref())
                .map_err(StoreError::Coordinate)?,
            kind,
            wall_ms: now_ms,
            clock,
            hash_chain: event.hash_chain.clone().unwrap_or_default(),
            disk_pos: disk_pos.clone(),
            global_sequence: global_seq,
        };
        self.index.insert(entry);
        debug!(event_id = %event.header.event_id, clock = clock, "append committed");

        // STEP 10: Broadcast notification to subscribers.
        self.subscribers.broadcast(&Notification {
            event_id: event.header.event_id,
            correlation_id,
            causation_id,
            coord: Coordinate::new(entity.as_ref(), scope.as_ref())
                .map_err(StoreError::Coordinate)?,
            kind,
            sequence: global_seq,
        });

        Ok(AppendReceipt {
            event_id: event.header.event_id,
            sequence: global_seq,
            disk_pos,
        })
    }
}

/// Find the latest segment ID by scanning data_dir for .fbat files.
fn find_latest_segment_id(dir: &std::path::Path) -> Option<u64> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.ends_with(".fbat") {
                name.trim_end_matches(".fbat").parse::<u64>().ok()
            } else {
                None
            }
        })
        .max()
}
